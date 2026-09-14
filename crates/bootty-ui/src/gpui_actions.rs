use std::{collections::HashMap, fmt::Write as _, rc::Rc};

use crate::keymap::{
    BindingElement, BindingFlags, BindingKey, BindingMods, BindingTrigger, parse_binding_elements,
};
use anyhow::{Context as _, Result, bail};
use bootty_config::config::InputConfig;
use bootty_config::config::MultiplexerBackendConfig;
use bootty_config::keymap_file::{KeymapAction, KeymapBindingKind};
use bootty_control::CommandInvocation;
use bootty_terminal::terminal_input_model::TerminalKey;
use gpui_kit::{
    Action, App, Global, KeyBinding, KeyBindingContextPredicate, NoAction, Subscription, Window,
};

use crate::app_actions::invocation_for_binding_action;
use crate::commands::CommandCatalog;
use crate::keymap_runtime::{
    KeymapFocus, KeymapSnapshot, context_is_active, invocation_for_action, invocations_match,
    parse_sequence_with_flags,
};

pub const WORKSPACE_KEY_CONTEXT: &str = "BoottyWorkspace";

/// Return a stable, GPUI-safe context name for one workspace window.
#[must_use]
pub fn workspace_key_context(window_state_key: &str) -> String {
    let mut context = String::with_capacity(
        WORKSPACE_KEY_CONTEXT
            .len()
            .saturating_add(window_state_key.len())
            .saturating_add(1),
    );
    context.push_str(WORKSPACE_KEY_CONTEXT);
    context.push('_');
    for byte in window_state_key.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' {
            context.push(char::from(byte));
        } else {
            // String formatting cannot fail.
            let _ = write!(context, "_{byte:02X}");
        }
    }
    context
}

#[derive(Clone, Debug, PartialEq, Eq, gpui_kit::Action)]
#[action(namespace = bootty, no_json)]
pub struct CycleApplicationWindow;

pub fn cycle_application_window(window: &mut Window, cx: &mut App) {
    let current = window.window_handle().window_id();
    let windows = cx.window_stack().unwrap_or_else(|| cx.windows());
    let Some(next) = windows
        .into_iter()
        .find(|candidate| candidate.window_id() != current)
    else {
        return;
    };
    let _ = next.update(cx, |_, window, _| window.activate_window());
}

#[derive(Clone)]
enum WorkspaceBindingAction {
    Invoke(CommandInvocation),
    Unbind,
}

impl WorkspaceBindingAction {
    fn into_action(self) -> Box<dyn Action> {
        match self {
            Self::Invoke(invocation) => Box::new(InvokeCommand::new(invocation)),
            Self::Unbind => Box::new(NoAction),
        }
    }
}

#[derive(Clone)]
struct WorkspaceBinding {
    keystrokes: String,
    action: WorkspaceBindingAction,
    command_context: bool,
}

#[derive(Clone, Default)]
pub struct WorkspaceKeyBindings {
    bindings: Vec<WorkspaceBinding>,
}

impl WorkspaceKeyBindings {
    fn new(bindings: impl IntoIterator<Item = WorkspaceBinding>) -> Self {
        Self {
            bindings: bindings.into_iter().collect(),
        }
    }

    #[must_use]
    pub fn command_hints(
        &self,
        catalog: &CommandCatalog,
    ) -> Vec<(crate::gpui::CommandAction, String)> {
        let mut seen = std::collections::HashSet::new();
        let mut hints = Vec::new();
        for binding in self.bindings.iter().rev() {
            if !binding.command_context || !seen.insert(&binding.keystrokes) {
                continue;
            }
            let WorkspaceBindingAction::Invoke(invocation) = &binding.action else {
                continue;
            };
            let Ok(resolved) = catalog.resolve(invocation.clone()) else {
                continue;
            };
            let crate::commands::CommandExecutor::Core(
                crate::commands::CoreCommandExecutor::Synchronous(
                    crate::commands::SynchronousCommand::Command(action),
                ),
            ) = resolved.executor
            else {
                continue;
            };
            if hints.iter().any(|(existing, _)| *existing == action) {
                continue;
            }
            let label = binding
                .keystrokes
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" > ");
            hints.push((action, label));
        }
        hints.reverse();
        hints
    }
}

struct ApplicationKeymap {
    base_bindings: Vec<KeyBinding>,
    workspace_bindings: HashMap<String, WorkspaceKeyBindings>,
    _keyboard_layout_subscription: Subscription,
}

impl Global for ApplicationKeymap {}

/// Replace Bootty's contextual bindings while preserving the complete component keymap.
///
/// # Errors
/// Returns a context or keystroke parsing error without replacing the current keymap.
pub fn replace_workspace_key_bindings(
    replacement: WorkspaceKeyBindings,
    cx: &mut App,
) -> Result<()> {
    replace_workspace_key_bindings_for_context(WORKSPACE_KEY_CONTEXT, replacement, cx)
}

/// Replace one window's contextual bindings without changing the bindings owned by other windows.
///
/// # Errors
/// Returns a context or keystroke parsing error without replacing the current keymap.
pub fn replace_workspace_key_bindings_for_context(
    context: &str,
    replacement: WorkspaceKeyBindings,
    cx: &mut App,
) -> Result<()> {
    ensure_application_keymap(cx);
    let mut workspace_bindings = cx.global::<ApplicationKeymap>().workspace_bindings.clone();
    workspace_bindings.insert(context.to_owned(), replacement);
    rebuild_application_keymap(workspace_bindings, cx)?;
    Ok(())
}

/// Remove a window's contextual bindings when its workspace entity is released.
pub fn remove_workspace_key_bindings(context: &str, cx: &mut App) {
    let Some(keymap) = cx.try_global::<ApplicationKeymap>() else {
        return;
    };
    let mut workspace_bindings = keymap.workspace_bindings.clone();
    if workspace_bindings.remove(context).is_some() {
        // A previously validated binding can only fail here if the active keyboard mapper changed
        // while the window was being released. The owning workspace is gone, so there is no useful
        // recovery action at this point.
        let _ = rebuild_application_keymap(workspace_bindings, cx);
    }
}

fn rebuild_application_keymap(
    workspace_bindings: HashMap<String, WorkspaceKeyBindings>,
    cx: &mut App,
) -> Result<()> {
    let base_bindings = cx.global::<ApplicationKeymap>().base_bindings.clone();
    let mut bindings = Vec::new();
    for (context, replacement) in &workspace_bindings {
        bindings.extend(load_workspace_key_bindings(replacement, context, cx)?);
    }

    cx.clear_key_bindings();
    cx.bind_keys(base_bindings);
    cx.bind_keys(bindings);
    cx.global_mut::<ApplicationKeymap>().workspace_bindings = workspace_bindings;
    Ok(())
}

fn ensure_application_keymap(cx: &mut App) {
    if cx.has_global::<ApplicationKeymap>() {
        return;
    }

    // The native host registers static component and application bindings first. From here this
    // owner rebuilds the complete keymap whenever a layout-dependent binding can change.
    let base_bindings = cx.key_bindings().borrow().bindings().cloned().collect();
    let keyboard_layout_subscription = cx.on_keyboard_layout_change(|cx| {
        let replacement = cx.global::<ApplicationKeymap>().workspace_bindings.clone();
        if let Err(error) = rebuild_application_keymap(replacement, cx) {
            // Rebuilding validates before publication, so the prior bindings remain installed.
            eprintln!(
                "Unable to rebuild workspace bindings after keyboard layout change: {error:#}"
            );
        }
    });
    cx.set_global(ApplicationKeymap {
        base_bindings,
        workspace_bindings: HashMap::new(),
        _keyboard_layout_subscription: keyboard_layout_subscription,
    });
}

fn load_workspace_key_bindings(
    bindings: &WorkspaceKeyBindings,
    context: &str,
    cx: &App,
) -> Result<Vec<KeyBinding>> {
    let workspace_context: Rc<KeyBindingContextPredicate> =
        KeyBindingContextPredicate::parse(context)
            .context("parse Bootty workspace key context")?
            .into();
    let command_context: Rc<KeyBindingContextPredicate> =
        KeyBindingContextPredicate::parse(&format!("{context} > Command"))
            .context("parse Bootty command key context")?
            .into();
    bindings
        .bindings
        .iter()
        .cloned()
        .map(|binding| {
            KeyBinding::load(
                &binding.keystrokes,
                binding.action.into_action(),
                Some(if binding.command_context {
                    command_context.clone()
                } else {
                    workspace_context.clone()
                }),
                true,
                None,
                cx.keyboard_mapper().as_ref(),
            )
            .with_context(|| format!("load GPUI key binding {:?}", binding.keystrokes))
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq, gpui_kit::Action)]
#[action(namespace = bootty, no_json)]
pub struct InvokeCommand {
    invocation: CommandInvocation,
}

impl InvokeCommand {
    #[must_use]
    pub const fn new(invocation: CommandInvocation) -> Self {
        Self { invocation }
    }

    #[must_use]
    pub const fn invocation(&self) -> &CommandInvocation {
        &self.invocation
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuiBindingSpec {
    pub keystrokes: String,
    pub invocation: CommandInvocation,
}

/// Project supported application bindings from the input configuration.
///
/// # Errors
/// Returns an invalid keybinding, unsupported action, or unsupported chain error.
pub fn binding_specs(input: &InputConfig) -> Result<Vec<GpuiBindingSpec>> {
    let mut specs = Vec::new();

    for entry in &input.keybind {
        let elements = parse_binding_elements(entry)
            .map_err(|error| anyhow::anyhow!("invalid keybind {entry:?}: {error:?}"))?;
        let mut leader = None;

        for element in elements {
            match element {
                BindingElement::Leader(trigger) => leader = Some(trigger),
                BindingElement::Binding(binding) => {
                    let invocation = invocation_for_binding_action(binding.action)
                        .with_context(|| format!("unsupported keybind {entry:?}"))?;
                    let pending_leader = leader.take();

                    if binding.flags != BindingFlags::default() {
                        continue;
                    }
                    let Some(trigger) = gpui_keystroke(&binding.trigger) else {
                        continue;
                    };
                    let keystrokes = match pending_leader {
                        Some(leader) => {
                            let Some(leader) = gpui_keystroke(&leader) else {
                                continue;
                            };
                            format!("{leader} {trigger}")
                        }
                        None => trigger,
                    };

                    specs.push(GpuiBindingSpec {
                        keystrokes,
                        invocation,
                    });
                }
                BindingElement::Chain(_) => {
                    bail!("chain keybinds are not supported for app-level keybind actions");
                }
            }
        }
    }

    Ok(specs)
}

/// Load application keybindings using the active keyboard mapper.
///
/// # Errors
/// Returns a configuration, context, or native keystroke parsing error.
pub fn key_bindings(input: &InputConfig, cx: &App) -> Result<Vec<KeyBinding>> {
    load_workspace_key_bindings(&workspace_key_bindings(input)?, WORKSPACE_KEY_CONTEXT, cx)
}

/// Prepare the configured application bindings for a workspace.
///
/// # Errors
/// Returns an invalid keybinding, unsupported action, or unsupported chain error.
pub fn workspace_key_bindings(input: &InputConfig) -> Result<WorkspaceKeyBindings> {
    Ok(WorkspaceKeyBindings::new(
        binding_specs(input)?
            .into_iter()
            .map(|spec| WorkspaceBinding {
                keystrokes: spec.keystrokes,
                action: WorkspaceBindingAction::Invoke(spec.invocation),
                command_context: false,
            }),
    ))
}

pub fn key_bindings_for_snapshot(
    snapshot: &KeymapSnapshot,
    focus: KeymapFocus,
    backend: MultiplexerBackendConfig,
    catalog: &CommandCatalog,
) -> WorkspaceKeyBindings {
    // Root's Tab bindings navigate controls. Mask those only in terminal focus; configured
    // bindings are appended afterward so an explicit user Tab binding still takes precedence.
    let terminal_keys = ["tab", "shift-tab"]
        .into_iter()
        .filter(move |_| focus == KeymapFocus::Terminal)
        .map(|keystrokes| WorkspaceBinding {
            keystrokes: keystrokes.to_owned(),
            action: WorkspaceBindingAction::Unbind,
            command_context: false,
        });
    // Mask component defaults before applying this window's editable Command bindings.
    // Otherwise removing a binding silently reveals the library's original shortcut.
    let command_keys = ["up", "down", "enter", "escape", "ctrl-p", "ctrl-n"]
        .into_iter()
        .filter(move |_| focus == KeymapFocus::Command)
        .map(|keystrokes| WorkspaceBinding {
            keystrokes: keystrokes.to_owned(),
            action: WorkspaceBindingAction::Unbind,
            command_context: true,
        });
    let mut effective = Vec::new();
    for binding in snapshot
        .effective_bindings
        .iter()
        .filter(|binding| context_is_active(&binding.context, focus, backend))
    {
        let Ok((sequence, flags)) = parse_sequence_with_flags(&binding.keystrokes) else {
            continue;
        };
        if flags != BindingFlags::default() {
            continue;
        }
        let Some(keystrokes) = sequence
            .iter()
            .map(gpui_keystroke)
            .collect::<Option<Vec<_>>>()
            .map(|keystrokes| keystrokes.join(" "))
        else {
            continue;
        };
        if binding.kind == KeymapBindingKind::Unbind {
            let Some(target) = invocation_for_action(&binding.action, catalog)
                .ok()
                .flatten()
            else {
                continue;
            };
            effective.retain(|existing: &WorkspaceBinding| {
                !(existing.keystrokes == keystrokes
                    && matches!(&existing.action, WorkspaceBindingAction::Invoke(invocation)
                        if invocations_match(&target, invocation)))
            });
            continue;
        }
        let action = if binding.action == KeymapAction::None {
            WorkspaceBindingAction::Unbind
        } else {
            let Some(invocation) = invocation_for_action(&binding.action, catalog)
                .ok()
                .flatten()
            else {
                continue;
            };
            WorkspaceBindingAction::Invoke(invocation)
        };
        effective.push(WorkspaceBinding {
            keystrokes,
            action,
            command_context: focus == KeymapFocus::Command,
        });
    }
    WorkspaceKeyBindings::new(terminal_keys.chain(command_keys).chain(effective))
}

fn gpui_keystroke(trigger: &BindingTrigger) -> Option<String> {
    if has_side_constraint(trigger.mods) {
        return None;
    }

    let mut key = match &trigger.key {
        BindingKey::Unicode(' ') => "space".to_owned(),
        BindingKey::Unicode(ch) if ch.is_ascii_uppercase() && !trigger.mods.shift => return None,
        BindingKey::Unicode(ch) => ch.to_lowercase().collect(),
        BindingKey::Physical(key) => gpui_physical_key(*key)?.to_owned(),
        BindingKey::ScrollUp | BindingKey::ScrollDown | BindingKey::CatchAll => return None,
    };
    let mut shift = trigger.mods.shift;
    if shift && let Some(shifted) = trigger.key.shifted_symbol_utf8() {
        // GPUI represents shifted symbols by their produced character on every platform. Bootty's
        // trigger keeps the user's Shift-plus-key spelling, and its key-text owner resolves it.
        shifted.clone_into(&mut key);
        shift = false;
    }

    let mut keystroke = String::new();
    push_modifier(&mut keystroke, trigger.mods.ctrl, "ctrl");
    push_modifier(&mut keystroke, trigger.mods.alt, "alt");
    push_modifier(&mut keystroke, shift, "shift");
    push_modifier(&mut keystroke, trigger.mods.command, "cmd");
    keystroke.push_str(&key);
    gpui_kit::Keystroke::parse(&keystroke).ok()?;
    Some(keystroke)
}

const fn has_side_constraint(mods: BindingMods) -> bool {
    mods.shift_side.is_some()
        || mods.ctrl_side.is_some()
        || mods.alt_side.is_some()
        || mods.command_side.is_some()
}

fn push_modifier(output: &mut String, enabled: bool, name: &str) {
    if enabled {
        output.push_str(name);
        output.push('-');
    }
}

const fn gpui_physical_key(key: TerminalKey) -> Option<&'static str> {
    Some(match key {
        TerminalKey::Backquote => "`",
        TerminalKey::Backslash => "\\",
        TerminalKey::BracketLeft => "[",
        TerminalKey::BracketRight => "]",
        TerminalKey::Comma => ",",
        TerminalKey::Digit0 => "0",
        TerminalKey::Digit1 => "1",
        TerminalKey::Digit2 => "2",
        TerminalKey::Digit3 => "3",
        TerminalKey::Digit4 => "4",
        TerminalKey::Digit5 => "5",
        TerminalKey::Digit6 => "6",
        TerminalKey::Digit7 => "7",
        TerminalKey::Digit8 => "8",
        TerminalKey::Digit9 => "9",
        TerminalKey::Equal => "=",
        TerminalKey::A => "a",
        TerminalKey::B => "b",
        TerminalKey::C => "c",
        TerminalKey::D => "d",
        TerminalKey::E => "e",
        TerminalKey::F => "f",
        TerminalKey::G => "g",
        TerminalKey::H => "h",
        TerminalKey::I => "i",
        TerminalKey::J => "j",
        TerminalKey::K => "k",
        TerminalKey::L => "l",
        TerminalKey::M => "m",
        TerminalKey::N => "n",
        TerminalKey::O => "o",
        TerminalKey::P => "p",
        TerminalKey::Q => "q",
        TerminalKey::R => "r",
        TerminalKey::S => "s",
        TerminalKey::T => "t",
        TerminalKey::U => "u",
        TerminalKey::V => "v",
        TerminalKey::W => "w",
        TerminalKey::X => "x",
        TerminalKey::Y => "y",
        TerminalKey::Z => "z",
        TerminalKey::Minus => "-",
        TerminalKey::Period => ".",
        TerminalKey::Quote => "'",
        TerminalKey::Semicolon => ";",
        TerminalKey::Slash => "/",
        TerminalKey::Enter => "enter",
        TerminalKey::Tab => "tab",
        TerminalKey::Backspace => "backspace",
        TerminalKey::Escape => "escape",
        TerminalKey::ArrowUp => "up",
        TerminalKey::ArrowDown => "down",
        TerminalKey::ArrowRight => "right",
        TerminalKey::ArrowLeft => "left",
        TerminalKey::Delete => "delete",
        TerminalKey::Home => "home",
        TerminalKey::End => "end",
        TerminalKey::PageUp => "pageup",
        TerminalKey::PageDown => "pagedown",
        TerminalKey::Space => "space",
        TerminalKey::Insert => "insert",
        TerminalKey::F1 => "f1",
        TerminalKey::F2 => "f2",
        TerminalKey::F3 => "f3",
        TerminalKey::F4 => "f4",
        TerminalKey::F5 => "f5",
        TerminalKey::F6 => "f6",
        TerminalKey::F7 => "f7",
        TerminalKey::F8 => "f8",
        TerminalKey::F9 => "f9",
        TerminalKey::F10 => "f10",
        TerminalKey::F11 => "f11",
        TerminalKey::F12 => "f12",
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

pub(crate) fn dock_binding_action(action: crate::commands::DockAction) -> InvokeCommand {
    InvokeCommand::new(CommandInvocation::from_action(
        action.command().action(),
        bootty_control::Caller::Keybinding,
    ))
}
