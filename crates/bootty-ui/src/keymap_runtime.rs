use std::{collections::HashSet, sync::Arc, time::Instant};

use crate::{
    gpui::InputEvent,
    keymap::{BindingFlags, BindingKey, BindingModSide, BindingMods, BindingTrigger},
};
use anyhow::Result;
use bootty_config::{
    KeymapMatch, KeymapPhysicalKey, KeymapTrigger, KeymapTriggerKey,
    config::{BoottyConfig, MultiplexerBackendConfig},
    keymap_file::{KeymapAction, KeymapBindingKind, KeymapContext, KeymapEdit, KeymapWriteOutcome},
};
use bootty_control::{Caller, CommandDescriptor, CommandInvocation, CommandOutcome};
use bootty_terminal::{
    terminal_input::ModifierSideState,
    terminal_input_model::{KeyInput, TerminalKey},
};
use gpui_kit::{KeyBindingContextPredicate, KeyContext};
use serde_json::Value;

use crate::{
    app_actions::{
        binding_triggers_for_key_input, binding_triggers_for_key_with_modifier_sides,
        binding_triggers_for_scroll_with_modifier_sides,
    },
    commands::CommandCatalog,
};

pub use bootty_config::keymap::{KeymapBindingSnapshot, KeymapBindingSource, KeymapSnapshot};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum KeymapFocus {
    Terminal,
    Sidebar,
    Command,
    #[default]
    Other,
}

pub(crate) struct ResolvedBinding {
    pub(crate) invocation: CommandInvocation,
    pub(crate) consumed: bool,
}

#[derive(Clone)]
struct RuntimeContext {
    predicate: Option<KeyBindingContextPredicate>,
}

pub struct KeymapRuntime {
    runtime: bootty_config::keymap::KeymapRuntime<CommandInvocation, RuntimeContext>,
    catalog: Arc<CommandCatalog>,
}

impl KeymapRuntime {
    pub fn new(config: &BoottyConfig, catalog: Arc<CommandCatalog>) -> Self {
        let runtime = bootty_config::keymap::KeymapRuntime::new(
            config,
            built_in_bindings(config),
            |action| invocation_for_action(action, &catalog),
            compile_context,
        );
        Self { runtime, catalog }
    }

    #[must_use]
    pub const fn snapshot(&self) -> &KeymapSnapshot {
        self.runtime.snapshot()
    }

    pub fn reload_due(&mut self, now: Instant) -> bool {
        self.runtime.reload_due(now)
    }

    pub fn sync_config(&mut self, config: &BoottyConfig) {
        let catalog = &self.catalog;
        self.runtime.sync_config(
            config,
            built_in_bindings(config),
            |action| invocation_for_action(action, catalog),
            compile_context,
        );
    }

    /// Reload the keymap file and report compilation diagnostics.
    ///
    /// # Errors
    /// Returns an error when the keymap file cannot be loaded.
    pub fn reload(&mut self) -> Result<Option<String>> {
        let catalog = &self.catalog;
        self.runtime
            .reload(
                |action| invocation_for_action(action, catalog),
                compile_context,
            )
            .map_err(Into::into)
    }

    /// Apply a keymap edit through the authoritative file owner.
    ///
    /// # Errors
    /// Returns validation, concurrency, and persistence errors from the keymap writer.
    pub fn edit(&mut self, edit: &KeymapEdit) -> Result<KeymapWriteOutcome> {
        let catalog = &self.catalog;
        self.runtime
            .edit(
                edit,
                |action| invocation_for_action(action, catalog),
                compile_context,
            )
            .map_err(Into::into)
    }

    pub(crate) fn split_events(
        &mut self,
        events: Vec<InputEvent>,
        modifier_sides: ModifierSideState,
        focus: KeymapFocus,
        backend: MultiplexerBackendConfig,
    ) -> (Vec<InputEvent>, Vec<CommandInvocation>) {
        let mut remaining = Vec::with_capacity(events.len());
        let mut invocations = Vec::new();
        let mut suppress_next_text = false;
        for event in events {
            if suppress_next_text && matches!(event, InputEvent::ImeCommit(_)) {
                continue;
            }
            if matches!(event, InputEvent::Key { pressed: false, .. }) {
                suppress_next_text = false;
            }
            let candidates = match &event {
                InputEvent::Key {
                    key,
                    pressed: true,
                    repeat: false,
                    modifiers,
                    ..
                } => binding_triggers_for_key_with_modifier_sides(*key, *modifiers, modifier_sides),
                InputEvent::MouseWheel {
                    delta, modifiers, ..
                } if delta.y != 0.0 => binding_triggers_for_scroll_with_modifier_sides(
                    delta.y > 0.0,
                    *modifiers,
                    modifier_sides,
                ),
                _ => Vec::new(),
            };
            let candidates = to_keymap_candidates(&candidates);
            let resolved = self.runtime.next_candidates(&candidates, |context| {
                runtime_context_is_active(context, focus, backend)
            });
            let resolved = resolved_to_binding(resolved, focus);
            if let Some(resolved) = resolved {
                if resolved.consumed && matches!(event, InputEvent::Key { .. }) {
                    suppress_next_text = true;
                }
                invocations.push(resolved.invocation);
                if !resolved.consumed {
                    remaining.push(event);
                }
            } else {
                remaining.push(event);
            }
        }
        (remaining, invocations)
    }

    pub(crate) fn invocation_for_input(
        &mut self,
        input: KeyInput,
        backend: MultiplexerBackendConfig,
    ) -> Option<ResolvedBinding> {
        let candidates = to_keymap_candidates(&binding_triggers_for_key_input(input));
        let resolved = self.runtime.next_candidates(&candidates, |context| {
            runtime_context_is_active(context, KeymapFocus::Terminal, backend)
        });
        resolved_to_binding(resolved, KeymapFocus::Terminal)
    }
}

fn resolved_to_binding(
    resolved: KeymapMatch<CommandInvocation>,
    focus: KeymapFocus,
) -> Option<ResolvedBinding> {
    match resolved {
        KeymapMatch::NoMatch => None,
        KeymapMatch::Pending | KeymapMatch::Consumed => Some(ResolvedBinding {
            invocation: consume_invocation(focus),
            consumed: true,
        }),
        KeymapMatch::Matched { action, consumed } => Some(ResolvedBinding {
            invocation: action,
            consumed,
        }),
    }
}

fn consume_invocation(focus: KeymapFocus) -> CommandInvocation {
    let command = if focus == KeymapFocus::Sidebar {
        "ui.sidebar.ignore"
    } else {
        "ignore"
    };
    CommandInvocation::from_action(command, Caller::Keybinding)
}

pub(crate) fn context_is_active(
    context: &KeymapContext,
    focus: KeymapFocus,
    backend: MultiplexerBackendConfig,
) -> bool {
    compile_context(context)
        .is_ok_and(|context| runtime_context_is_active(&context, focus, backend))
}

fn compile_context(context: &KeymapContext) -> Result<RuntimeContext, String> {
    let source = match context {
        KeymapContext::Global => return Ok(RuntimeContext { predicate: None }),
        KeymapContext::Sidebar => "Sidebar",
        KeymapContext::Command => "Command",
        KeymapContext::Terminal => "Terminal",
        KeymapContext::Herdr => "Terminal && backend == herdr",
        KeymapContext::Native => "Terminal && backend == native",
        KeymapContext::Rmux => "Terminal && backend == rmux",
        KeymapContext::Tmux => "Terminal && backend == tmux",
        KeymapContext::Expression(source) => source,
    };
    KeyBindingContextPredicate::parse(source)
        .map(|predicate| RuntimeContext {
            predicate: Some(predicate),
        })
        .map_err(|error| format!("invalid keybinding context {source:?}: {error}"))
}

fn runtime_context_is_active(
    context: &RuntimeContext,
    focus: KeymapFocus,
    backend: MultiplexerBackendConfig,
) -> bool {
    let Some(predicate) = &context.predicate else {
        return focus != KeymapFocus::Command;
    };
    predicate
        .depth_of(&active_key_contexts(focus, backend))
        .is_some()
}

fn active_key_contexts(focus: KeymapFocus, backend: MultiplexerBackendConfig) -> Vec<KeyContext> {
    let mut workspace = KeyContext::new_with_defaults();
    workspace.add("Workspace");
    let mut contexts = vec![workspace];
    let mut focused = KeyContext::default();
    match focus {
        KeymapFocus::Terminal => {
            focused.add("Terminal");
            focused.set("backend", backend.to_string().to_ascii_lowercase());
        }
        KeymapFocus::Sidebar => focused.add("Sidebar"),
        KeymapFocus::Command => focused.add("Command"),
        KeymapFocus::Other => return contexts,
    }
    if focus == KeymapFocus::Command {
        contexts.clear();
    }
    contexts.push(focused);
    contexts
}

/// Compatibility adapter for the GPUI registration layer while its trigger types remain native.
pub(crate) fn parse_sequence_with_flags(
    source: &str,
) -> Result<(Vec<BindingTrigger>, BindingFlags), String> {
    let sequence =
        bootty_config::parse_keymap_sequence(source).map_err(|error| error.to_string())?;
    let triggers = sequence
        .triggers
        .iter()
        .map(from_keymap_trigger)
        .collect::<Vec<_>>();
    Ok((
        triggers,
        BindingFlags {
            consumed: sequence.flags.consumed,
            all: sequence.flags.all,
            global: sequence.flags.global,
            performable: sequence.flags.performable,
        },
    ))
}

fn to_keymap_trigger(trigger: &BindingTrigger) -> Option<KeymapTrigger> {
    let key = match &trigger.key {
        BindingKey::Unicode(ch) => KeymapTriggerKey::Unicode(*ch),
        BindingKey::Physical(key) => KeymapTriggerKey::Physical(to_keymap_physical(*key)?),
        BindingKey::ScrollUp => KeymapTriggerKey::ScrollUp,
        BindingKey::ScrollDown => KeymapTriggerKey::ScrollDown,
        BindingKey::CatchAll => KeymapTriggerKey::CatchAll,
    };
    Some(KeymapTrigger {
        modifiers: to_keymap_modifiers(trigger.mods),
        key,
    })
}

fn to_keymap_candidates(candidates: &[BindingTrigger]) -> Vec<KeymapTrigger> {
    let mut converted = candidates
        .iter()
        .filter_map(to_keymap_trigger)
        .collect::<Vec<_>>();
    let modifiers = converted
        .iter()
        .map(|candidate| candidate.modifiers)
        .collect::<Vec<_>>();
    for modifiers in modifiers {
        let catch_all = KeymapTrigger {
            modifiers,
            key: KeymapTriggerKey::CatchAll,
        };
        if !converted.contains(&catch_all) {
            converted.push(catch_all);
        }
    }
    if !converted.is_empty() {
        let catch_all = KeymapTrigger {
            modifiers: bootty_config::KeymapModifiers::default(),
            key: KeymapTriggerKey::CatchAll,
        };
        if !converted.contains(&catch_all) {
            converted.push(catch_all);
        }
    }
    converted
}

pub(crate) fn config_keymap_candidates(candidates: &[BindingTrigger]) -> Vec<KeymapTrigger> {
    to_keymap_candidates(candidates)
}

fn from_keymap_trigger(trigger: &KeymapTrigger) -> BindingTrigger {
    let key = match &trigger.key {
        KeymapTriggerKey::Unicode(ch) => BindingKey::Unicode(*ch),
        KeymapTriggerKey::Physical(key) => BindingKey::Physical(from_keymap_physical(*key)),
        KeymapTriggerKey::ScrollUp => BindingKey::ScrollUp,
        KeymapTriggerKey::ScrollDown => BindingKey::ScrollDown,
        KeymapTriggerKey::CatchAll => BindingKey::CatchAll,
    };
    BindingTrigger {
        mods: from_keymap_modifiers(trigger.modifiers),
        key,
    }
}

fn to_keymap_modifiers(mods: BindingMods) -> bootty_config::KeymapModifiers {
    bootty_config::KeymapModifiers {
        shift: mods.shift,
        ctrl: mods.ctrl,
        alt: mods.alt,
        command: mods.command,
        shift_side: mods.shift_side.map(to_keymap_side),
        ctrl_side: mods.ctrl_side.map(to_keymap_side),
        alt_side: mods.alt_side.map(to_keymap_side),
        command_side: mods.command_side.map(to_keymap_side),
    }
}

fn from_keymap_modifiers(mods: bootty_config::KeymapModifiers) -> BindingMods {
    BindingMods {
        shift: mods.shift,
        ctrl: mods.ctrl,
        alt: mods.alt,
        command: mods.command,
        shift_side: mods.shift_side.map(from_keymap_side),
        ctrl_side: mods.ctrl_side.map(from_keymap_side),
        alt_side: mods.alt_side.map(from_keymap_side),
        command_side: mods.command_side.map(from_keymap_side),
    }
}

const fn to_keymap_side(side: BindingModSide) -> bootty_config::KeymapModifierSide {
    match side {
        BindingModSide::Left => bootty_config::KeymapModifierSide::Left,
        BindingModSide::Right => bootty_config::KeymapModifierSide::Right,
    }
}

const fn from_keymap_side(side: bootty_config::KeymapModifierSide) -> BindingModSide {
    match side {
        bootty_config::KeymapModifierSide::Left => BindingModSide::Left,
        bootty_config::KeymapModifierSide::Right => BindingModSide::Right,
    }
}

const fn to_keymap_physical(key: TerminalKey) -> Option<KeymapPhysicalKey> {
    Some(match key {
        TerminalKey::Backquote => KeymapPhysicalKey::Backquote,
        TerminalKey::Backslash => KeymapPhysicalKey::Backslash,
        TerminalKey::BracketLeft => KeymapPhysicalKey::BracketLeft,
        TerminalKey::BracketRight => KeymapPhysicalKey::BracketRight,
        TerminalKey::Comma => KeymapPhysicalKey::Comma,
        TerminalKey::Digit0 => KeymapPhysicalKey::Digit0,
        TerminalKey::Digit1 => KeymapPhysicalKey::Digit1,
        TerminalKey::Digit2 => KeymapPhysicalKey::Digit2,
        TerminalKey::Digit3 => KeymapPhysicalKey::Digit3,
        TerminalKey::Digit4 => KeymapPhysicalKey::Digit4,
        TerminalKey::Digit5 => KeymapPhysicalKey::Digit5,
        TerminalKey::Digit6 => KeymapPhysicalKey::Digit6,
        TerminalKey::Digit7 => KeymapPhysicalKey::Digit7,
        TerminalKey::Digit8 => KeymapPhysicalKey::Digit8,
        TerminalKey::Digit9 => KeymapPhysicalKey::Digit9,
        TerminalKey::Equal => KeymapPhysicalKey::Equal,
        TerminalKey::A => KeymapPhysicalKey::A,
        TerminalKey::B => KeymapPhysicalKey::B,
        TerminalKey::C => KeymapPhysicalKey::C,
        TerminalKey::D => KeymapPhysicalKey::D,
        TerminalKey::E => KeymapPhysicalKey::E,
        TerminalKey::F => KeymapPhysicalKey::F,
        TerminalKey::G => KeymapPhysicalKey::G,
        TerminalKey::H => KeymapPhysicalKey::H,
        TerminalKey::I => KeymapPhysicalKey::I,
        TerminalKey::J => KeymapPhysicalKey::J,
        TerminalKey::K => KeymapPhysicalKey::K,
        TerminalKey::L => KeymapPhysicalKey::L,
        TerminalKey::M => KeymapPhysicalKey::M,
        TerminalKey::N => KeymapPhysicalKey::N,
        TerminalKey::O => KeymapPhysicalKey::O,
        TerminalKey::P => KeymapPhysicalKey::P,
        TerminalKey::Q => KeymapPhysicalKey::Q,
        TerminalKey::R => KeymapPhysicalKey::R,
        TerminalKey::S => KeymapPhysicalKey::S,
        TerminalKey::T => KeymapPhysicalKey::T,
        TerminalKey::U => KeymapPhysicalKey::U,
        TerminalKey::V => KeymapPhysicalKey::V,
        TerminalKey::W => KeymapPhysicalKey::W,
        TerminalKey::X => KeymapPhysicalKey::X,
        TerminalKey::Y => KeymapPhysicalKey::Y,
        TerminalKey::Z => KeymapPhysicalKey::Z,
        TerminalKey::Minus => KeymapPhysicalKey::Minus,
        TerminalKey::Period => KeymapPhysicalKey::Period,
        TerminalKey::Quote => KeymapPhysicalKey::Quote,
        TerminalKey::Semicolon => KeymapPhysicalKey::Semicolon,
        TerminalKey::Slash => KeymapPhysicalKey::Slash,
        TerminalKey::ArrowUp => KeymapPhysicalKey::ArrowUp,
        TerminalKey::ArrowDown => KeymapPhysicalKey::ArrowDown,
        TerminalKey::ArrowRight => KeymapPhysicalKey::ArrowRight,
        TerminalKey::ArrowLeft => KeymapPhysicalKey::ArrowLeft,
        TerminalKey::Delete => KeymapPhysicalKey::Delete,
        TerminalKey::Home => KeymapPhysicalKey::Home,
        TerminalKey::End => KeymapPhysicalKey::End,
        TerminalKey::PageUp => KeymapPhysicalKey::PageUp,
        TerminalKey::PageDown => KeymapPhysicalKey::PageDown,
        TerminalKey::Space => KeymapPhysicalKey::Space,
        TerminalKey::Insert => KeymapPhysicalKey::Insert,
        TerminalKey::Enter => KeymapPhysicalKey::Enter,
        TerminalKey::Tab => KeymapPhysicalKey::Tab,
        TerminalKey::Backspace => KeymapPhysicalKey::Backspace,
        TerminalKey::Escape => KeymapPhysicalKey::Escape,
        TerminalKey::F1 => KeymapPhysicalKey::F1,
        TerminalKey::F2 => KeymapPhysicalKey::F2,
        TerminalKey::F3 => KeymapPhysicalKey::F3,
        TerminalKey::F4 => KeymapPhysicalKey::F4,
        TerminalKey::F5 => KeymapPhysicalKey::F5,
        TerminalKey::F6 => KeymapPhysicalKey::F6,
        TerminalKey::F7 => KeymapPhysicalKey::F7,
        TerminalKey::F8 => KeymapPhysicalKey::F8,
        TerminalKey::F9 => KeymapPhysicalKey::F9,
        TerminalKey::F10 => KeymapPhysicalKey::F10,
        TerminalKey::F11 => KeymapPhysicalKey::F11,
        TerminalKey::F12 => KeymapPhysicalKey::F12,
        TerminalKey::Numpad0
        | TerminalKey::Numpad1
        | TerminalKey::Numpad2
        | TerminalKey::Numpad3
        | TerminalKey::Numpad4
        | TerminalKey::Numpad5
        | TerminalKey::Numpad6
        | TerminalKey::Numpad7
        | TerminalKey::Numpad8
        | TerminalKey::Numpad9
        | TerminalKey::NumpadAdd
        | TerminalKey::NumpadDecimal
        | TerminalKey::NumpadDivide
        | TerminalKey::NumpadEnter
        | TerminalKey::NumpadEqual
        | TerminalKey::NumpadMultiply
        | TerminalKey::NumpadSubtract
        | TerminalKey::ShiftLeft
        | TerminalKey::ShiftRight
        | TerminalKey::ControlLeft
        | TerminalKey::ControlRight
        | TerminalKey::AltLeft
        | TerminalKey::AltRight => return None,
    })
}

const fn from_keymap_physical(key: KeymapPhysicalKey) -> TerminalKey {
    match key {
        KeymapPhysicalKey::Backquote => TerminalKey::Backquote,
        KeymapPhysicalKey::Backslash => TerminalKey::Backslash,
        KeymapPhysicalKey::BracketLeft => TerminalKey::BracketLeft,
        KeymapPhysicalKey::BracketRight => TerminalKey::BracketRight,
        KeymapPhysicalKey::Comma => TerminalKey::Comma,
        KeymapPhysicalKey::Digit0 => TerminalKey::Digit0,
        KeymapPhysicalKey::Digit1 => TerminalKey::Digit1,
        KeymapPhysicalKey::Digit2 => TerminalKey::Digit2,
        KeymapPhysicalKey::Digit3 => TerminalKey::Digit3,
        KeymapPhysicalKey::Digit4 => TerminalKey::Digit4,
        KeymapPhysicalKey::Digit5 => TerminalKey::Digit5,
        KeymapPhysicalKey::Digit6 => TerminalKey::Digit6,
        KeymapPhysicalKey::Digit7 => TerminalKey::Digit7,
        KeymapPhysicalKey::Digit8 => TerminalKey::Digit8,
        KeymapPhysicalKey::Digit9 => TerminalKey::Digit9,
        KeymapPhysicalKey::Equal => TerminalKey::Equal,
        KeymapPhysicalKey::A => TerminalKey::A,
        KeymapPhysicalKey::B => TerminalKey::B,
        KeymapPhysicalKey::C => TerminalKey::C,
        KeymapPhysicalKey::D => TerminalKey::D,
        KeymapPhysicalKey::E => TerminalKey::E,
        KeymapPhysicalKey::F => TerminalKey::F,
        KeymapPhysicalKey::G => TerminalKey::G,
        KeymapPhysicalKey::H => TerminalKey::H,
        KeymapPhysicalKey::I => TerminalKey::I,
        KeymapPhysicalKey::J => TerminalKey::J,
        KeymapPhysicalKey::K => TerminalKey::K,
        KeymapPhysicalKey::L => TerminalKey::L,
        KeymapPhysicalKey::M => TerminalKey::M,
        KeymapPhysicalKey::N => TerminalKey::N,
        KeymapPhysicalKey::O => TerminalKey::O,
        KeymapPhysicalKey::P => TerminalKey::P,
        KeymapPhysicalKey::Q => TerminalKey::Q,
        KeymapPhysicalKey::R => TerminalKey::R,
        KeymapPhysicalKey::S => TerminalKey::S,
        KeymapPhysicalKey::T => TerminalKey::T,
        KeymapPhysicalKey::U => TerminalKey::U,
        KeymapPhysicalKey::V => TerminalKey::V,
        KeymapPhysicalKey::W => TerminalKey::W,
        KeymapPhysicalKey::X => TerminalKey::X,
        KeymapPhysicalKey::Y => TerminalKey::Y,
        KeymapPhysicalKey::Z => TerminalKey::Z,
        KeymapPhysicalKey::Minus => TerminalKey::Minus,
        KeymapPhysicalKey::Period => TerminalKey::Period,
        KeymapPhysicalKey::Quote => TerminalKey::Quote,
        KeymapPhysicalKey::Semicolon => TerminalKey::Semicolon,
        KeymapPhysicalKey::Slash => TerminalKey::Slash,
        KeymapPhysicalKey::ArrowUp => TerminalKey::ArrowUp,
        KeymapPhysicalKey::ArrowDown => TerminalKey::ArrowDown,
        KeymapPhysicalKey::ArrowRight => TerminalKey::ArrowRight,
        KeymapPhysicalKey::ArrowLeft => TerminalKey::ArrowLeft,
        KeymapPhysicalKey::Delete => TerminalKey::Delete,
        KeymapPhysicalKey::Home => TerminalKey::Home,
        KeymapPhysicalKey::End => TerminalKey::End,
        KeymapPhysicalKey::PageUp => TerminalKey::PageUp,
        KeymapPhysicalKey::PageDown => TerminalKey::PageDown,
        KeymapPhysicalKey::Space => TerminalKey::Space,
        KeymapPhysicalKey::Insert => TerminalKey::Insert,
        KeymapPhysicalKey::Enter => TerminalKey::Enter,
        KeymapPhysicalKey::Tab => TerminalKey::Tab,
        KeymapPhysicalKey::Backspace => TerminalKey::Backspace,
        KeymapPhysicalKey::Escape => TerminalKey::Escape,
        KeymapPhysicalKey::F1 => TerminalKey::F1,
        KeymapPhysicalKey::F2 => TerminalKey::F2,
        KeymapPhysicalKey::F3 => TerminalKey::F3,
        KeymapPhysicalKey::F4 => TerminalKey::F4,
        KeymapPhysicalKey::F5 => TerminalKey::F5,
        KeymapPhysicalKey::F6 => TerminalKey::F6,
        KeymapPhysicalKey::F7 => TerminalKey::F7,
        KeymapPhysicalKey::F8 => TerminalKey::F8,
        KeymapPhysicalKey::F9 => TerminalKey::F9,
        KeymapPhysicalKey::F10 => TerminalKey::F10,
        KeymapPhysicalKey::F11 => TerminalKey::F11,
        KeymapPhysicalKey::F12 => TerminalKey::F12,
    }
}

pub(crate) fn invocation_for_action(
    action: &KeymapAction,
    catalog: &CommandCatalog,
) -> Result<Option<CommandInvocation>, String> {
    let KeymapAction::Command { name, input } = action else {
        return Ok(None);
    };
    let invocation = if let Some(input) = input {
        let descriptor = catalog
            .describe(name)
            .ok_or_else(|| format!("unknown Bootty command {name:?}"))?;
        CommandInvocation::new(
            name,
            arguments_for_input(&descriptor, input)?,
            Caller::Keybinding,
        )
    } else {
        CommandInvocation::from_action(name, Caller::Keybinding)
    };
    catalog
        .resolve(invocation.clone())
        .map_err(command_outcome_message)?;
    Ok(Some(invocation))
}

pub(crate) fn invocations_match(left: &CommandInvocation, right: &CommandInvocation) -> bool {
    left.command == right.command && left.arguments == right.arguments
}

fn arguments_for_input(
    descriptor: &CommandDescriptor,
    input: &Value,
) -> Result<Vec<String>, String> {
    match input {
        Value::Null => Ok(Vec::new()),
        Value::Array(values) => values.iter().map(json_argument).collect(),
        Value::Object(values) => {
            let expected = descriptor
                .arguments
                .arguments
                .iter()
                .map(|argument| argument.name.as_str())
                .collect::<HashSet<_>>();
            if let Some(unknown) = values.keys().find(|name| !expected.contains(name.as_str())) {
                return Err(format!(
                    "command {} has no argument named {unknown:?}",
                    descriptor.id
                ));
            }
            descriptor
                .arguments
                .arguments
                .iter()
                .filter_map(|argument| values.get(&argument.name))
                .map(json_argument)
                .collect()
        }
        value => Ok(vec![json_argument(value)?]),
    }
}

fn json_argument(value: &Value) -> Result<String, String> {
    match value {
        Value::String(value) => Ok(value.clone()),
        Value::Number(value) => Ok(value.to_string()),
        Value::Bool(value) => Ok(value.to_string()),
        Value::Null => Ok("null".to_owned()),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string(value).map_err(|error| error.to_string())
        }
    }
}

fn command_outcome_message(outcome: CommandOutcome) -> String {
    match outcome {
        CommandOutcome::Failed { message, .. }
        | CommandOutcome::Unsupported { message }
        | CommandOutcome::Unavailable { message }
        | CommandOutcome::Denied { message }
        | CommandOutcome::StaleTarget { message } => message,
        CommandOutcome::Success { .. } | CommandOutcome::ConfirmationRequired { .. } => {
            "unexpected command validation result".to_owned()
        }
    }
}

fn built_in_bindings(config: &BoottyConfig) -> Vec<KeymapBindingSnapshot> {
    let mut bindings = Vec::new();
    for (keystrokes, command) in [
        ("arrow_up", "ui.command.previous"),
        ("ctrl+p", "ui.command.previous"),
        ("arrow_down", "ui.command.next"),
        ("ctrl+n", "ui.command.next"),
        ("enter", "ui.command.confirm"),
        ("escape", "ui.command.cancel"),
        ("ctrl+shift+f", "ui.command.toggle_favorite"),
    ] {
        bindings.push(KeymapBindingSnapshot {
            context: KeymapContext::Command,
            keystrokes: keystrokes.to_owned(),
            action: KeymapAction::command(command),
            kind: KeymapBindingKind::Binding,
            source: KeymapBindingSource::BuiltIn,
        });
    }
    bindings.extend(bootty_config::legacy_keymap_bindings(config));
    bindings
}
