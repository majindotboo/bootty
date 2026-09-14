use crate::gpui::{InputEvent, Key, Modifiers, Point, PointerButton, WheelUnit};
use bootty_mux::terminal::TerminalRuntime;
use bootty_terminal::geometry::{SurfacePoint, TerminalSurface, ViewTransform};
use bootty_terminal::terminal_input::{DirectKeyInput, ModifierSideState};
use bootty_terminal::terminal_input_model::{
    KeyInput, KeyMods, MacosOptionAsAlt, MouseAction, MouseButton, MouseInput,
};
use num_traits::ToPrimitive as _;
use std::path::PathBuf;

use super::recorded_chord::normalize_recorded_chord;
use super::{AppEffect, AppState, PendingMouseInputTarget, ViewportSnapshot};
use crate::app_actions::{SidebarAction, builtin_app_invocation_for_direct_key};
use crate::error_catalog::ErrorNotice;
use crate::input::{TerminalInputCommand, focus::InputFocus, router::route_events};
use crate::presentation::dialogs::CommandPaletteDialog;
use crate::terminal_interaction::TerminalInteractionInput;
use bootty_mux::workspace::ScopedSessionTarget;
#[derive(Clone, Debug, PartialEq, Eq)]
enum LocalFileHandoff {
    Ready(String),
    Rejected(ErrorNotice),
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct NeutralWheelScrollState {
    point_remainder_y: f32,
    line_remainder_y: f32,
}

impl AppState {
    pub fn sidebar_focused(&self) -> bool {
        self.input_focus == InputFocus::Sidebar
    }
    pub const fn terminal_focused(&self) -> bool {
        self.direct_terminal_input_enabled()
    }
    pub const fn sidebar_hovered_session(&self) -> Option<&ScopedSessionTarget> {
        self.sidebar_hovered_session.as_ref()
    }
    pub const fn direct_input_suppresses_host_events(&self) -> bool {
        self.direct_terminal_input_enabled()
    }

    pub fn drain_direct_input(&mut self) {
        if let Some(rx) = &self.modifier_side_rx
            && let Some(latest) = rx.try_iter().last()
        {
            self.modifier_sides = latest;
        }
        let Some(rx) = &self.direct_input_rx else {
            return;
        };
        self.pending_direct_input.extend(rx.try_iter());
    }
    pub(crate) fn queue_direct_input(&mut self, mut input: DirectKeyInput) {
        self.modifier_sides.apply_to_key_input(&mut input.input);
        self.pending_direct_input.push(input);
    }
    pub(super) const fn effective_terminal_cursor_icon(&self) -> super::CursorIcon {
        if self.mouse_pointer_hidden_while_typing {
            super::CursorIcon::None
        } else {
            self.terminal_cursor_icon
        }
    }
    pub(super) fn set_mouse_pointer_hidden_while_typing(
        &mut self,
        hidden: bool,
        effects: &mut Vec<AppEffect>,
    ) {
        let hidden = hidden && self.config().input.hide_mouse_pointer_while_typing;
        if self.mouse_pointer_hidden_while_typing == hidden {
            return;
        }
        self.mouse_pointer_hidden_while_typing = hidden;
        effects.push(AppEffect::SetTerminalCursorIcon(
            self.effective_terminal_cursor_icon(),
        ));
    }
    pub(super) fn hide_mouse_pointer_for_terminal_typing(&mut self, effects: &mut Vec<AppEffect>) {
        self.set_mouse_pointer_hidden_while_typing(true, effects);
    }
    pub(super) fn restore_mouse_pointer_after_pointer_moved(
        &mut self,
        events: &[InputEvent],
        hover_pos: Option<Point>,
        effects: &mut Vec<AppEffect>,
    ) {
        let moved_by_event = events
            .iter()
            .any(|event| matches!(event, InputEvent::PointerMoved(_)));
        let moved_by_hover_pos = hover_pos.is_some() && hover_pos != self.last_mouse_hover_pos;
        self.last_mouse_hover_pos = hover_pos;

        if moved_by_event || moved_by_hover_pos {
            self.set_mouse_pointer_hidden_while_typing(false, effects);
        }
    }
    pub fn pending_direct_input(&self) -> &[DirectKeyInput] {
        &self.pending_direct_input
    }

    /// The modifier keys held right now, with their left/right sides, as tracked by the direct
    /// direct input path. The settings recorder needs this for wheel steps, which arrive as GPUI
    /// events with side-less modifiers.
    pub const fn modifier_sides(&self) -> ModifierSideState {
        self.modifier_sides
    }

    /// Drain the pending direct-input chords as binding-trigger strings for the settings keybind
    /// recorder. This is how the recorder captures cmd-modified chords like ⌘V and ⌘⌥X: GPUI
    /// exposes logical keys without modifier-side identity, but Bootty's direct input path
    /// keeps the full key + modifiers. Only meaningful while settings is open (the terminal is not
    /// consuming this input).
    pub fn take_settings_capture_chords(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending_direct_input)
            .into_iter()
            .map(|direct| {
                let chord = crate::keymap::BindingTrigger::from_key_input_with_modifier_sides(
                    direct.input(),
                )
                .format_entry();
                normalize_recorded_chord(chord)
            })
            .collect()
    }
    pub(super) const fn direct_terminal_input_enabled(&self) -> bool {
        self.input_focus.terminal_owns_input() && !self.dialogs.has_modal()
    }
    pub(super) fn handle_input(
        &mut self,
        events: Vec<InputEvent>,
        modifiers: Modifiers,
        hover_pos: Option<Point>,
        pressed_mouse_button: Option<PointerButton>,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        if events.contains(&InputEvent::WindowFocused(false)) {
            self.mouse_input_capture = None;
        }
        if self.pending_mouse_input_targets.is_empty() {
            return self.handle_input_batch(
                events,
                modifiers,
                hover_pos,
                (pressed_mouse_button, None),
                viewport,
                effects,
            );
        }
        let mut targets = std::mem::take(&mut self.pending_mouse_input_targets);
        let mut count = 0_usize;
        let mut held = self.mouse_input_capture.as_ref().map(|(_, button)| *button);
        let mut hover = hover_pos;
        let mut non_pointer_events = Vec::new();
        for event in events {
            let pointer = matches!(
                event,
                InputEvent::PointerMoved(_)
                    | InputEvent::PointerButton { .. }
                    | InputEvent::MouseWheel { .. }
            );
            let mut target = pointer.then(|| targets.pop_front().flatten()).flatten();
            let position = match &event {
                InputEvent::PointerMoved(position) | InputEvent::PointerButton { position, .. } => {
                    Some(*position)
                }
                _ => target.as_ref().and_then(|target| target.position).or(hover),
            };
            if !pointer {
                if matches!(
                    event,
                    InputEvent::PointerGone | InputEvent::WindowFocused(false)
                ) {
                    self.mouse_input_capture = None;
                    held = None;
                }
                non_pointer_events.push(event);
                continue;
            }
            if !non_pointer_events.is_empty() {
                count = count.saturating_add(self.handle_input_batch(
                    std::mem::take(&mut non_pointer_events),
                    modifiers,
                    hover,
                    (held, None),
                    viewport,
                    effects,
                ));
            }
            hover = position;
            match &event {
                InputEvent::PointerButton {
                    button,
                    pressed: true,
                    ..
                } => {
                    if let Some(target) = &target {
                        self.mouse_input_capture = Some((target.clone(), *button));
                    }
                    held = Some(*button);
                }
                InputEvent::PointerMoved(_) | InputEvent::PointerButton { pressed: false, .. } => {
                    if let Some((captured, _)) = &self.mouse_input_capture {
                        target = Some(captured.clone());
                    }
                }
                _ => {}
            }
            let release = matches!(event, InputEvent::PointerButton { pressed: false, .. });
            // Pointer events outside the pane are ignored, except captured drag/release.
            if target.is_some() {
                count = count.saturating_add(self.handle_input_batch(
                    vec![event],
                    modifiers,
                    position,
                    (held, target),
                    viewport,
                    effects,
                ));
            }
            if release {
                self.mouse_input_capture = None;
                held = None;
            }
        }
        if !non_pointer_events.is_empty() {
            count = count.saturating_add(self.handle_input_batch(
                non_pointer_events,
                modifiers,
                hover,
                (held, None),
                viewport,
                effects,
            ));
        }
        count
    }

    fn handle_input_batch(
        &mut self,
        events: Vec<InputEvent>,
        modifiers: Modifiers,
        hover_pos: Option<Point>,
        pointer: (Option<PointerButton>, Option<PendingMouseInputTarget>),
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        let (pressed_mouse_button, target) = pointer;
        if target
            .as_ref()
            .is_some_and(|target| target.owner != self.mouse_input_owner())
        {
            self.mouse_input_capture = None;
            return 0;
        }
        let terminal_input_enabled = self.direct_terminal_input_enabled();
        let copy_on_select = self.config().input.copy_on_select;
        let (surface, view, pane_id) = target.map_or(
            (self.terminal_surface, self.terminal_view_transform, None),
            |target| (Some(target.surface), target.view, target.pane_id),
        );
        let input_focus = self.input_focus;
        let outcome = self.terminal_interaction.handle_input(
            self.workspace.active.binding.terminal_mut(),
            TerminalInteractionInput {
                events,
                modifiers,
                pressed_mouse_button,
                input_focus,
                terminal_input_enabled,
                surface,
                view,
                chrome_handle_rects: &self.chrome_handle_rects,
                copy_on_select,
            },
        );
        let count = outcome.handled_count;
        effects.extend(outcome.effects);
        self.apply_terminal_outcome(outcome.last_error, outcome.focus_intent);

        let mut events = outcome.events;
        // `cmd+shift+,` over a palette row jumps to that command's keybinding editor.
        // Consume it here so it does not also fire its own global binding.
        if self.take_configure_keybind_chord(&mut events) {
            let action = self
                .dialogs
                .command_palette()
                .and_then(CommandPaletteDialog::current_action)
                .map(str::to_owned);
            self.close_overlay_dialogs();
            self.input_focus = InputFocus::Terminal;
            if let Some(action) = action {
                effects.push(AppEffect::ConfigureKeybind(action));
            }
        }
        let (events, actions) = self.split_app_actions(events);
        let routed = route_events(self.input_focus, events);
        let sidebar_count = self.handle_sidebar_input(&routed.ui_events, viewport, effects);
        let terminal_events = if terminal_input_enabled
            || (!self.dialogs.has_modal() && self.terminal_interaction.find_dialog().is_some())
        {
            routed.terminal_events
        } else {
            Vec::new()
        };
        let macos_option_as_alt = bootty_mux::terminal_config::terminal_macos_option_as_alt(
            self.config().input.macos_option_as_alt,
        );
        let commands = terminal_input_commands(
            terminal_events,
            modifiers,
            self.modifier_sides,
            hover_pos,
            pressed_mouse_button,
            surface,
            view,
            macos_option_as_alt,
            &mut self.wheel_scroll_state,
            &self.config_runtime,
        );
        let count = count
            .saturating_add(commands.len())
            .saturating_add(actions.len())
            .saturating_add(sidebar_count);
        for invocation in actions {
            let _ = self.dispatch_command(invocation, viewport, effects);
        }
        for command in commands {
            let pane_id = matches!(
                &command,
                TerminalInputCommand::Mouse(_) | TerminalInputCommand::MouseWheel { .. }
            )
            .then_some(pane_id.as_deref())
            .flatten();
            self.apply_terminal_input_to_target(command, effects, pane_id);
        }
        count
    }
    pub(super) fn handle_dropped_file_paths(&mut self, paths: &[PathBuf]) -> usize {
        if !self.direct_terminal_input_enabled() {
            return 0;
        }
        if paths.is_empty() {
            return 0;
        }
        if self.workspace.active.binding.multiplexer().remote.is_some() {
            self.record_notice(crate::error_catalog::ErrorNotice::RemoteFileHandoffUnsupported);
            return 0;
        }
        let text = match local_file_handoff(paths) {
            LocalFileHandoff::Ready(text) => text,
            LocalFileHandoff::Rejected(notice) => {
                self.record_notice(notice);
                return 0;
            }
        };
        if let Err(error) = self
            .workspace
            .active
            .binding
            .terminal_mut()
            .write_paste(&text)
        {
            self.record_error(error);
            return 0;
        }
        1
    }
    pub(super) fn handle_direct_input(
        &mut self,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) -> usize {
        self.drain_direct_input();
        let inputs = std::mem::take(&mut self.pending_direct_input);
        let count = inputs.len();
        if count == 0 {
            return 0;
        }
        if !self.direct_terminal_input_enabled() {
            return count;
        }

        let mut copy_mode_active =
            match TerminalRuntime::copy_mode_active(self.workspace.active.binding.terminal_mut()) {
                Ok(active) => active,
                Err(error) => {
                    self.record_error(error);
                    false
                }
            };
        for input in inputs {
            let mut input = input.input();
            input.mods = self.config_runtime.remap_mods(input.mods);
            let interaction = self.terminal_interaction.handle_direct_input(
                self.workspace.active.binding.terminal_mut(),
                input,
                copy_mode_active,
            );
            copy_mode_active = interaction.copy_mode_active;
            effects.extend(interaction.effects);
            self.apply_terminal_outcome(interaction.last_error, interaction.focus_intent);
            if interaction.consumed {
                continue;
            }
            if let Some(resolved) = self
                .keymap_runtime
                .invocation_for_input(input, self.workspace.multiplexer_backend())
            {
                let invocation = resolved.invocation;
                let _ = self.dispatch_command(invocation, viewport, effects);
                if resolved.consumed {
                    continue;
                }
            } else if let Some(invocation) = builtin_app_invocation_for_direct_key(input) {
                let _ = self.dispatch_command(invocation, viewport, effects);
                continue;
            }
            if copy_mode_active {
                continue;
            }
            self.apply_terminal_input(TerminalInputCommand::Key(input), effects);
        }
        count
    }
    fn handle_sidebar_input(
        &mut self,
        events: &[InputEvent],
        _viewport: ViewportSnapshot,
        _effects: &mut Vec<AppEffect>,
    ) -> usize {
        if self.input_focus != InputFocus::Sidebar {
            return 0;
        }
        self.ensure_sidebar_hovered_session();
        events.len()
    }
    pub(crate) fn focus_sidebar(&mut self) {
        self.input_focus = InputFocus::Sidebar;
        self.ensure_sidebar_hovered_session();
    }

    pub(crate) fn apply_sidebar_action(&mut self, action: SidebarAction) -> bool {
        match action {
            SidebarAction::Ignore => {}
            SidebarAction::PreviousSession => self.move_sidebar_hover(-1),
            SidebarAction::NextSession => self.move_sidebar_hover(1),
            SidebarAction::ActivateSession => return self.activate_sidebar_hovered_session(),
            SidebarAction::FocusTerminal => self.input_focus = InputFocus::Terminal,
        }
        true
    }
    fn ensure_sidebar_hovered_session(&mut self) {
        let targets = self.session_navigation_targets();
        if self
            .sidebar_hovered_session
            .as_ref()
            .is_some_and(|hovered| targets.contains(hovered))
        {
            return;
        }
        self.sidebar_hovered_session = self
            .workspace
            .active
            .binding
            .mux()
            .selected_session()
            .and_then(|selected| self.session_target_matching(selected))
            .or_else(|| targets.into_iter().next());
    }
    fn move_sidebar_hover(&mut self, delta: isize) {
        self.ensure_sidebar_hovered_session();
        let targets = self.session_navigation_targets();
        let Some(current) = self
            .sidebar_hovered_session
            .as_ref()
            .and_then(|hovered| targets.iter().position(|target| target == hovered))
        else {
            return;
        };
        let (Ok(current), Ok(count)) = (isize::try_from(current), isize::try_from(targets.len()))
        else {
            return;
        };
        let Some(next) = current
            .checked_add(delta)
            .and_then(|next| next.checked_rem_euclid(count))
            .and_then(|next| usize::try_from(next).ok())
        else {
            return;
        };
        self.sidebar_hovered_session = targets.get(next).cloned();
    }
    fn activate_sidebar_hovered_session(&mut self) -> bool {
        self.ensure_sidebar_hovered_session();
        let activated = self.sidebar_hovered_session.clone().is_some_and(|target| {
            let unclaimed = target.scope == self.workspace.active.binding.scope()
                && self
                    .unclaimed_sessions()
                    .iter()
                    .any(|session| session.session_id == target.session_id);
            if unclaimed {
                self.adopt_and_activate_scoped_session(&target)
            } else {
                self.activate_scoped_session_from_ui(&target)
            }
        });
        self.input_focus = InputFocus::Terminal;
        activated
    }
    pub(super) fn session_navigation_targets(&self) -> Vec<ScopedSessionTarget> {
        let mut targets = self
            .binding_session_groups()
            .into_iter()
            .flat_map(|group| {
                group
                    .sessions
                    .into_iter()
                    .map(move |session| ScopedSessionTarget::new(group.scope, session.id))
            })
            .collect::<Vec<_>>();
        targets.extend(self.unclaimed_sessions().into_iter().map(|session| {
            ScopedSessionTarget::new(self.workspace.active.binding.scope(), session.session_id)
        }));
        targets
    }
    pub(super) fn session_target_matching(&self, value: &str) -> Option<ScopedSessionTarget> {
        self.workspace
            .active
            .binding
            .mux()
            .session_by_id_or_name(value)
            .map(|session| {
                ScopedSessionTarget::new(self.workspace.active.binding.scope(), session.id.clone())
            })
    }
    pub(crate) fn apply_terminal_input(
        &mut self,
        command: TerminalInputCommand,
        effects: &mut Vec<AppEffect>,
    ) {
        self.apply_terminal_input_to_target(command, effects, None);
    }

    fn apply_terminal_input_to_target(
        &mut self,
        command: TerminalInputCommand,
        effects: &mut Vec<AppEffect>,
        pane_id: Option<&str>,
    ) {
        let terminal: &mut dyn TerminalRuntime = match pane_id {
            Some(pane_id) => {
                let Some(terminal) = self
                    .workspace
                    .active
                    .binding
                    .terminal_mut()
                    .focused_terminal_runtime(pane_id)
                else {
                    // A stale native hit target must not be sent to another pane.
                    return;
                };
                terminal
            }
            None => self.workspace.active.binding.terminal_mut(),
        };
        let (result, hides_pointer) = Self::apply_terminal_input_to_runtime(terminal, command);
        if let Err(error) = result {
            self.record_error(error);
        } else if hides_pointer {
            self.hide_mouse_pointer_for_terminal_typing(effects);
        }
    }

    fn apply_terminal_input_to_runtime(
        terminal: &mut dyn TerminalRuntime,
        command: TerminalInputCommand,
    ) -> (anyhow::Result<()>, bool) {
        match command {
            TerminalInputCommand::Text(text) => (terminal.write_input(text.as_bytes()), true),
            TerminalInputCommand::Paste(text) => (terminal.write_paste(&text), false),
            TerminalInputCommand::Focus(focused) => (terminal.encode_focus(focused), false),
            TerminalInputCommand::Key(input) => (terminal.encode_key(input), true),
            TerminalInputCommand::Mouse(input) => (terminal.encode_mouse(input), false),
            TerminalInputCommand::MouseWheel {
                input,
                scroll_delta,
            } => (terminal.handle_mouse_wheel(input, scroll_delta), false),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn terminal_input_commands(
    events: Vec<InputEvent>,
    modifiers: Modifiers,
    modifier_sides: ModifierSideState,
    hover_pos: Option<Point>,
    pressed_mouse_button: Option<PointerButton>,
    surface: Option<TerminalSurface>,
    view: ViewTransform,
    macos_option_as_alt: MacosOptionAsAlt,
    wheel_state: &mut NeutralWheelScrollState,
    config: &crate::config_runtime::AppConfigRuntime,
) -> Vec<TerminalInputCommand> {
    let mut commands = Vec::with_capacity(events.len());
    let suppress_modified_text =
        suppress_modified_text(&events, modifiers, macos_option_as_alt, modifier_sides);

    let mut held = initial_mouse_button(&events, pressed_mouse_button);

    for event in events {
        match event {
            InputEvent::ImeCommit(text) if !suppress_modified_text => {
                commands.push(TerminalInputCommand::Text(text));
            }
            InputEvent::WindowFocused(focused) => {
                commands.push(TerminalInputCommand::Focus(focused));
            }
            InputEvent::PointerMoved(pos) => {
                if let Some(input) = mouse_input_with_view(
                    pos,
                    MouseAction::Motion,
                    held,
                    modifiers,
                    surface,
                    view,
                    false,
                ) {
                    commands.push(TerminalInputCommand::Mouse(input));
                }
            }
            InputEvent::PointerButton {
                position,
                button,
                pressed,
                modifiers,
                ..
            } => {
                let button = terminal_mouse_button(button);
                let action = if pressed {
                    MouseAction::Press
                } else {
                    MouseAction::Release
                };
                let was_held = held;
                held = pressed.then_some(button);
                let input = mouse_input_with_view(
                    position,
                    action,
                    Some(button),
                    modifiers,
                    surface,
                    view,
                    !pressed && was_held == Some(button),
                );
                if let Some(input) = input {
                    commands.push(TerminalInputCommand::Mouse(input));
                }
            }
            InputEvent::MouseWheel {
                unit,
                delta,
                modifiers,
                ..
            } => {
                commands.extend(terminal_wheel_input(
                    delta.y,
                    unit,
                    hover_pos,
                    modifiers,
                    surface,
                    view,
                    wheel_state,
                ));
            }
            InputEvent::Key {
                key,
                pressed: true,
                repeat,
                modifiers,
            } => {
                commands.extend(terminal_key_input(
                    key,
                    repeat,
                    modifiers,
                    macos_option_as_alt,
                    modifier_sides,
                    config,
                ));
            }
            InputEvent::ImeCommit(_)
            | InputEvent::ModifiersChanged(_)
            | InputEvent::PointerGone
            | InputEvent::ImePreedit { .. }
            | InputEvent::Key { .. } => {}
        }
    }

    commands
}

fn initial_mouse_button(
    events: &[InputEvent],
    pressed_mouse_button: Option<PointerButton>,
) -> Option<MouseButton> {
    // `pressed_mouse_button` is the state after the whole frame. Do not treat motion before this
    // frame's first press as a drag, but retain the snapshot for release-only frames.
    let opens_with_press = events
        .iter()
        .find_map(|event| match event {
            InputEvent::PointerButton { pressed, .. } => Some(*pressed),
            _ => None,
        })
        .unwrap_or(false);
    if opens_with_press {
        None
    } else {
        pressed_mouse_button.map(terminal_mouse_button)
    }
}

fn suppress_modified_text(
    events: &[InputEvent],
    modifiers: Modifiers,
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
) -> bool {
    std::iter::once(modifiers)
        .chain(events.iter().filter_map(|event| match event {
            InputEvent::Key {
                pressed: true,
                modifiers,
                ..
            } => Some(*modifiers),
            _ => None,
        }))
        .any(|modifiers| {
            text_modifiers_are_suppressed(modifiers, macos_option_as_alt, modifier_sides)
        })
}

fn terminal_wheel_input(
    delta_y: f32,
    unit: WheelUnit,
    hover_pos: Option<Point>,
    modifiers: Modifiers,
    surface: Option<TerminalSurface>,
    view: ViewTransform,
    wheel_state: &mut NeutralWheelScrollState,
) -> Option<TerminalInputCommand> {
    let pos = hover_pos?;
    let button = mouse_wheel_button_from_delta_y(delta_y)?;
    let scroll_delta = mouse_wheel_scroll_delta(delta_y, unit, surface, wheel_state);
    if scroll_delta == 0 {
        return None;
    }
    let input = mouse_input_with_view(
        pos,
        MouseAction::Press,
        Some(button),
        modifiers,
        surface,
        view,
        false,
    )?;
    Some(TerminalInputCommand::MouseWheel {
        input,
        scroll_delta,
    })
}

fn terminal_key_input(
    key: Key,
    repeat: bool,
    modifiers: Modifiers,
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
    config: &crate::config_runtime::AppConfigRuntime,
) -> Option<TerminalInputCommand> {
    let terminal_key = crate::app_actions::terminal_key(key)?;
    if !should_encode_key(terminal_key, modifiers, macos_option_as_alt, modifier_sides) {
        return None;
    }
    let mut input = KeyInput {
        key: terminal_key,
        mods: crate::app_actions::key_mods_for_binding(modifiers, modifier_sides),
        repeat,
        utf8: key_utf8(key, modifiers.shift),
        unshifted: key_unshifted(key),
    };
    input.mods = config.remap_mods(input.mods);
    Some(TerminalInputCommand::Key(input))
}

const fn should_encode_key(
    key: bootty_terminal::terminal_input_model::TerminalKey,
    modifiers: Modifiers,
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
) -> bool {
    is_control_key(key)
        || modifiers.control
        || (modifiers.alt && option_alt_is_meta(macos_option_as_alt, modifier_sides))
}

const fn text_modifiers_are_suppressed(
    modifiers: Modifiers,
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
) -> bool {
    modifiers.control
        || modifiers.platform
        || (modifiers.alt && option_alt_is_meta(macos_option_as_alt, modifier_sides))
}

const fn option_alt_is_meta(
    macos_option_as_alt: MacosOptionAsAlt,
    modifier_sides: ModifierSideState,
) -> bool {
    match macos_option_as_alt {
        MacosOptionAsAlt::None => false,
        MacosOptionAsAlt::Both => true,
        MacosOptionAsAlt::Left => modifier_sides.left_alt || !modifier_sides.right_alt,
        MacosOptionAsAlt::Right => modifier_sides.right_alt || !modifier_sides.left_alt,
    }
}

const fn is_control_key(key: bootty_terminal::terminal_input_model::TerminalKey) -> bool {
    use bootty_terminal::terminal_input_model::TerminalKey;
    matches!(
        key,
        TerminalKey::Enter
            | TerminalKey::Tab
            | TerminalKey::Backspace
            | TerminalKey::Escape
            | TerminalKey::Insert
            | TerminalKey::Delete
            | TerminalKey::Home
            | TerminalKey::End
            | TerminalKey::PageUp
            | TerminalKey::PageDown
            | TerminalKey::ArrowUp
            | TerminalKey::ArrowDown
            | TerminalKey::ArrowRight
            | TerminalKey::ArrowLeft
            | TerminalKey::F1
            | TerminalKey::F2
            | TerminalKey::F3
            | TerminalKey::F4
            | TerminalKey::F5
            | TerminalKey::F6
            | TerminalKey::F7
            | TerminalKey::F8
            | TerminalKey::F9
            | TerminalKey::F10
            | TerminalKey::F11
            | TerminalKey::F12
    )
}

const fn key_utf8(key: Key, shifted: bool) -> Option<&'static str> {
    let (plain, shifted_value) = match key {
        Key::Space => (" ", " "),
        Key::Backtick => ("`", "~"),
        Key::Backslash | Key::Pipe => ("\\", "|"),
        Key::OpenBracket | Key::OpenCurlyBracket => ("[", "{"),
        Key::CloseBracket | Key::CloseCurlyBracket => ("]", "}"),
        Key::Comma => (",", "<"),
        Key::Digit(0) => ("0", ")"),
        Key::Digit(1) | Key::ExclamationMark => ("1", "!"),
        Key::Digit(2) => ("2", "@"),
        Key::Digit(3) => ("3", "#"),
        Key::Digit(4) => ("4", "$"),
        Key::Digit(5) => ("5", "%"),
        Key::Digit(6) => ("6", "^"),
        Key::Digit(7) => ("7", "&"),
        Key::Digit(8) => ("8", "*"),
        Key::Digit(9) => ("9", "("),
        Key::Plus | Key::Equals => ("=", "+"),
        Key::Minus => ("-", "_"),
        Key::Period => (".", ">"),
        Key::Quote => ("'", "\""),
        Key::Semicolon | Key::Colon => (";", ":"),
        Key::Slash | Key::QuestionMark => ("/", "?"),
        Key::Letter('a') => ("a", "A"),
        Key::Letter('b') => ("b", "B"),
        Key::Letter('c') => ("c", "C"),
        Key::Letter('d') => ("d", "D"),
        Key::Letter('e') => ("e", "E"),
        Key::Letter('f') => ("f", "F"),
        Key::Letter('g') => ("g", "G"),
        Key::Letter('h') => ("h", "H"),
        Key::Letter('i') => ("i", "I"),
        Key::Letter('j') => ("j", "J"),
        Key::Letter('k') => ("k", "K"),
        Key::Letter('l') => ("l", "L"),
        Key::Letter('m') => ("m", "M"),
        Key::Letter('n') => ("n", "N"),
        Key::Letter('o') => ("o", "O"),
        Key::Letter('p') => ("p", "P"),
        Key::Letter('q') => ("q", "Q"),
        Key::Letter('r') => ("r", "R"),
        Key::Letter('s') => ("s", "S"),
        Key::Letter('t') => ("t", "T"),
        Key::Letter('u') => ("u", "U"),
        Key::Letter('v') => ("v", "V"),
        Key::Letter('w') => ("w", "W"),
        Key::Letter('x') => ("x", "X"),
        Key::Letter('y') => ("y", "Y"),
        Key::Letter('z') => ("z", "Z"),
        _ => return None,
    };
    Some(if shifted { shifted_value } else { plain })
}

fn key_unshifted(key: Key) -> Option<char> {
    key_utf8(key, false)?.chars().next()
}

const fn terminal_mouse_button(button: PointerButton) -> MouseButton {
    match button {
        PointerButton::Left => MouseButton::Left,
        PointerButton::Right => MouseButton::Right,
        PointerButton::Middle => MouseButton::Middle,
        PointerButton::Back => MouseButton::Four,
        PointerButton::Forward => MouseButton::Five,
    }
}

fn mouse_wheel_button_from_delta_y(delta_y: f32) -> Option<MouseButton> {
    match delta_y.partial_cmp(&0.0) {
        Some(std::cmp::Ordering::Greater) => Some(MouseButton::Four),
        Some(std::cmp::Ordering::Less) => Some(MouseButton::Five),
        Some(std::cmp::Ordering::Equal) | None => None,
    }
}

fn mouse_mods(modifiers: Modifiers) -> KeyMods {
    let mut mods =
        crate::app_actions::key_mods_for_binding(modifiers, ModifierSideState::default());
    mods.command = false;
    mods
}

fn mouse_input_with_view(
    pos: Point,
    action: MouseAction,
    button: Option<MouseButton>,
    modifiers: Modifiers,
    surface: Option<TerminalSurface>,
    view: ViewTransform,
    clamped: bool,
) -> Option<MouseInput> {
    let surface = surface?;
    let mut pos = view.inverse_point(surface_point(pos));
    if clamped {
        pos.x = pos.x.clamp(surface.rect.min_x, surface.rect.max_x);
        pos.y = pos.y.clamp(surface.rect.min_y, surface.rect.max_y);
    }
    let pixel_position = surface.relative_position(pos)?;
    let position = surface.mouse_position(pos)?;
    Some(MouseInput {
        action,
        button,
        mods: mouse_mods(modifiers),
        x: position.x,
        y: position.y,
        pixel_x: pixel_position.x,
        pixel_y: pixel_position.y,
        size: surface.mouse_metrics().into(),
    })
}

fn mouse_wheel_scroll_delta(
    delta_y: f32,
    unit: WheelUnit,
    surface: Option<TerminalSurface>,
    wheel_state: &mut NeutralWheelScrollState,
) -> isize {
    let (remainder, divisor) = match unit {
        WheelUnit::Pixels => {
            let cell_height = surface.map_or(16.0, |surface| surface.cell.height).max(1.0);
            (&mut wheel_state.point_remainder_y, cell_height)
        }
        WheelUnit::Lines => (&mut wheel_state.line_remainder_y, 1.0),
    };
    if !delta_y.is_finite() {
        return 0;
    }
    *remainder += delta_y;
    let whole = (*remainder / divisor).trunc();
    if whole == 0.0 {
        return 0;
    }
    *remainder = if whole.is_finite() {
        f32::mul_add(whole, -divisor, *remainder)
    } else {
        0.0
    };
    whole
        .to_isize()
        .unwrap_or_else(|| {
            if whole.is_sign_negative() {
                isize::MIN
            } else {
                isize::MAX
            }
        })
        .saturating_neg()
}

const fn surface_point(point: Point) -> SurfacePoint {
    SurfacePoint {
        x: point.x,
        y: point.y,
    }
}

fn local_file_handoff(paths: &[PathBuf]) -> LocalFileHandoff {
    if paths.iter().any(|path| !path.exists()) {
        return LocalFileHandoff::Rejected(ErrorNotice::FileHandoffRejected(
            "file handoff rejected: local path is unavailable".to_owned(),
        ));
    }
    crate::file_paths::format_file_paths_for_paste(paths.iter().map(PathBuf::as_path)).map_or_else(
        || {
            LocalFileHandoff::Rejected(ErrorNotice::FileHandoffRejected(
                "file handoff rejected: unsupported local path".to_owned(),
            ))
        },
        LocalFileHandoff::Ready,
    )
}
