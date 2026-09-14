use crate::{
    gpui::{InputEvent, Key},
    keymap::CopyToClipboard,
};
use bootty_config::config::WindowConfig;

use super::{AppEffect, AppState, CursorIcon, ViewportSnapshot};
use crate::app_actions::{
    AppAction, FontSizeAction, KeybindAction, MuxKeyAction, TerminalFindAction,
    TerminalScrollAction,
};
use crate::input::focus::InputFocus;
use crate::platform::macos_handles_non_native_fullscreen_frame;
use crate::terminal_config::terminal_text_config;
impl AppState {
    pub(super) fn take_configure_keybind_chord(&self, events: &mut Vec<InputEvent>) -> bool {
        if !self.dialogs.is_command_palette() {
            return false;
        }
        let macos = cfg!(target_os = "macos");
        let Some(index) = events.iter().position(|event| {
            matches!(
                event,
                InputEvent::Key {
                    key: Key::Comma,
                    pressed: true,
                    modifiers,
                    ..
                } if if macos {
                    modifiers.shift && modifiers.platform
                        && !modifiers.alt && !modifiers.control
                } else {
                    modifiers.shift && modifiers.control && !modifiers.alt
                }
            )
        }) else {
            return false;
        };
        events.remove(index);
        true
    }
    pub(super) fn apply_keybind_action(
        &mut self,
        action: KeybindAction,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        match action {
            KeybindAction::App(action) => self.apply_app_action(action, viewport, effects),
            KeybindAction::OpenSetting(id) => {
                if self
                    .settings_schema()
                    .allows_write_path(&id.split('.').collect::<Vec<_>>())
                {
                    effects.push(AppEffect::OpenSetting(id));
                } else {
                    self.record_error(format!("Unknown setting: {id}"));
                }
            }
            KeybindAction::Mux(action) => {
                self.apply_mux_key_action(action);
                effects.push(AppEffect::RequestRepaint);
            }
            KeybindAction::Scroll(action) => self.apply_terminal_scroll_action(action),
            KeybindAction::Write(bytes) => {
                if let Err(error) = self
                    .workspace
                    .active
                    .binding
                    .terminal_mut()
                    .write_input(&bytes)
                {
                    self.record_error(error);
                } else {
                    self.hide_mouse_pointer_for_terminal_typing(effects);
                }
            }
            KeybindAction::Font(action) => self.apply_font_size_action(action, effects),
            KeybindAction::Find(action) => self.apply_terminal_find_action(action, effects),
            KeybindAction::CopyToClipboard(format) => {
                self.copy_terminal_selection_or_request_copy(format, effects);
            }
            KeybindAction::CopyMode => {
                self.enter_terminal_copy_mode(effects);
            }
            KeybindAction::PasteFromClipboard => {
                self.commands
                    .queue(bootty_control::CommandInvocation::from_action(
                        "paste_from_clipboard",
                        bootty_control::Caller::Internal,
                    ));
            }
        }
    }

    fn apply_app_action(
        &mut self,
        action: AppAction,
        viewport: ViewportSnapshot,
        effects: &mut Vec<AppEffect>,
    ) {
        match action {
            AppAction::ReloadConfig => {
                self.reload_config(effects);
            }
            AppAction::Ignore => {}
            AppAction::NewWindow => effects.push(AppEffect::OpenWindow),
            AppAction::NewMuxSession => {
                self.open_new_mux_session_dialog();
            }

            AppAction::SessionPicker => {
                self.toggle_session_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::CommandPalette => {
                self.open_command_palette_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::ChangeAppearance(mode) => {
                self.persist_appearance_mode(mode, effects);
            }
            AppAction::SwitchTheme => {
                self.open_theme_picker_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::RenameSession => {
                self.open_rename_session_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::MoveSessionToSpace => {
                self.open_space_picker_for_current_session();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::RenameTab => {
                self.open_rename_tab_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::DitchSession => {
                self.open_ditch_session_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::EditSpace => {
                self.open_edit_space_dialog_from_ui(self.workspace.active.id);
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::Quit => effects.push(AppEffect::QuitApplication),
            AppAction::CreateSpace => {
                self.open_create_space_dialog_from_ui();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::CloseSpace => {
                if !self.close_space_from_ui(self.workspace.active.id) {
                    self.record_notice(crate::error_catalog::ErrorNotice::LastSpaceCannotClose);
                }
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::NextSpace => {
                if self.activate_relative_space(1) {
                    effects.push(AppEffect::RequestRepaint);
                }
            }
            AppAction::PreviousSpace => {
                if self.activate_relative_space(-1) {
                    effects.push(AppEffect::RequestRepaint);
                }
            }
            AppAction::SelectSpace(index) => {
                if self.select_space(index) {
                    effects.push(AppEffect::RequestRepaint);
                }
            }
            AppAction::ShowKeybinds => {
                self.open_keybind_help_dialog();
                effects.push(AppEffect::RequestRepaint);
            }
            AppAction::Close => effects.push(AppEffect::CloseWindow),
            AppAction::EditTheme => self.open_theme_editor(),
            AppAction::ExportTerminal => self.open_capture_dialog(),
            AppAction::Dock(action) => {
                effects.push(AppEffect::Dock(crate::commands::DockRequest::local(action)));
            }
            AppAction::OpenSettings => effects.push(AppEffect::OpenSettings),
            AppAction::ToggleFullscreen => self.toggle_fullscreen(viewport, effects),
            AppAction::FocusTerminal => {
                self.close_overlay_dialogs();
                self.input_focus = InputFocus::Terminal;
                effects.push(AppEffect::FocusTerminal);
            }
            AppAction::ToggleSidebarFocus => self.toggle_sidebar_focus(effects),
            AppAction::ToggleSidebarVisibility => {
                effects.push(AppEffect::Dock(crate::commands::DockRequest::local(
                    crate::commands::DockAction::TogglePanel(
                        bootty_config::config::PanelKind::Sessions,
                    ),
                )));
            }
        }
    }

    fn toggle_fullscreen(&mut self, viewport: ViewportSnapshot, effects: &mut Vec<AppEffect>) {
        let enabled = if should_toggle_native_fullscreen(&self.config().window) {
            !viewport.fullscreen
        } else {
            next_non_native_fullscreen_state(
                macos_handles_non_native_fullscreen_frame(&self.config().window),
                self.macos_non_native_fullscreen_active,
                viewport.maximized,
            )
        };
        self.set_runtime_fullscreen(enabled, effects);
    }

    fn toggle_sidebar_focus(&mut self, effects: &mut Vec<AppEffect>) {
        self.close_overlay_dialogs();
        if self.input_focus == InputFocus::Sidebar {
            self.input_focus = InputFocus::Terminal;
            effects.push(AppEffect::FocusTerminal);
        } else {
            effects.push(AppEffect::Dock(crate::commands::DockRequest::local(
                crate::commands::DockAction::Sidebar,
            )));
            self.input_focus = InputFocus::Sidebar;
            self.sidebar_hovered_session = self
                .workspace
                .active
                .binding
                .mux()
                .selected_session()
                .and_then(|selected| self.session_target_matching(selected))
                .or_else(|| self.session_navigation_targets().into_iter().next());
        }
        effects.push(AppEffect::RequestRepaint);
    }

    fn enter_terminal_copy_mode(&mut self, effects: &mut Vec<AppEffect>) {
        let outcome = self
            .terminal_interaction
            .enter_copy_mode(self.workspace.active.binding.terminal_mut());
        if let Some(error) = outcome.last_error {
            self.record_error(error);
        }
        effects.extend(outcome.effects);
    }
    fn copy_terminal_selection_or_request_copy(
        &mut self,
        format: CopyToClipboard,
        effects: &mut Vec<AppEffect>,
    ) {
        let outcome = self
            .terminal_interaction
            .copy_selection_or_request(self.workspace.active.binding.terminal_mut(), format);
        if let Some(error) = outcome.last_error {
            self.record_error(error);
        }
        effects.extend(outcome.effects);
    }
    pub(super) fn apply_session_navigation_action(&mut self, action: MuxKeyAction) -> bool {
        let target = match action {
            MuxKeyAction::SelectSession(index) => self
                .workspace
                .active
                .binding
                .mux()
                .sessions()
                .get(usize::try_from(index.saturating_sub(1)).unwrap_or(usize::MAX))
                .map(|session| session.id.clone()),
            MuxKeyAction::NextSession => self.relative_session(true),
            MuxKeyAction::PreviousSession => self.relative_session(false),
            MuxKeyAction::LastSession => self
                .workspace
                .active
                .binding
                .mux()
                .previous_selected_session()
                .map(str::to_owned),
            // Not a session-navigation action: let the caller route it.
            _ => return false,
        };
        // Missing navigation targets are handled no-ops; these actions belong to Bootty,
        // so they must not fall through to the backend command builder.
        if let Some(target) = target {
            if let Err(error) = self.workspace.activate_target(
                self.workspace.active.binding.scope(),
                &target,
                None,
                &self.repaint,
            ) {
                self.record_error(error);
            } else {
                self.sync_terminal_panes_now();
            }
        }
        true
    }
    fn relative_session(&self, forward: bool) -> Option<String> {
        let sessions = self.workspace.active.binding.mux().sessions();
        if sessions.is_empty() {
            return None;
        }
        let selected = self.workspace.active.binding.mux().selected_session();
        let current = selected
            .and_then(|selected| {
                sessions
                    .iter()
                    .position(|session| session.id == selected || session.name == selected)
            })
            .unwrap_or(0);
        let next = if forward {
            current.saturating_add(1).checked_rem(sessions.len())?
        } else {
            current
                .checked_sub(1)
                .unwrap_or_else(|| sessions.len().saturating_sub(1))
        };
        sessions.get(next).map(|session| session.id.clone())
    }
    fn apply_terminal_find_action(
        &mut self,
        action: TerminalFindAction,
        effects: &mut Vec<AppEffect>,
    ) {
        if self.terminal_interaction.find_action_opens_dialog(&action) {
            self.close_overlay_dialogs();
        }
        let focused_pane_id = self.focused_pane();
        let outcome = self.terminal_interaction.apply_find_action(
            self.workspace.active.binding.terminal_mut(),
            action,
            focused_pane_id.as_deref(),
        );
        effects.extend(outcome.effects);
        self.apply_terminal_outcome(outcome.last_error, outcome.focus_intent);
    }
    fn apply_terminal_scroll_action(&mut self, action: TerminalScrollAction) {
        let terminal = self.workspace.active.binding.terminal_mut();
        let page_rows = isize::try_from(terminal.grid_size().1).unwrap_or(isize::MAX);
        let result = match action {
            TerminalScrollAction::Top => terminal.scroll_viewport_to(0),
            TerminalScrollAction::Bottom => terminal.scroll_viewport_to(usize::MAX),
            TerminalScrollAction::PageUp => {
                terminal.scroll_viewport_delta(page_rows.saturating_neg())
            }
            TerminalScrollAction::PageDown => terminal.scroll_viewport_delta(page_rows),
            TerminalScrollAction::Lines(lines) => {
                terminal.scroll_viewport_delta(isize::from(lines))
            }
        };
        if let Err(error) = result {
            self.record_error(error);
        }
    }
    fn apply_font_size_action(&mut self, action: FontSizeAction, effects: &mut Vec<AppEffect>) {
        let default_size = self.config_runtime.configured_font_size();
        let current_size = self.config().font.size;
        let next_size = match action {
            FontSizeAction::Increase(delta) => current_size + delta,
            FontSizeAction::Decrease(delta) => current_size - delta,
            FontSizeAction::Reset => default_size,
            FontSizeAction::Set(size) => size,
        }
        .max(1.0);
        if (next_size - current_size).abs() <= f32::EPSILON {
            return;
        }
        self.config_runtime.set_font_size(next_size);
        let text_config = terminal_text_config(&self.config().font);
        if let Some(existing) = effects.iter_mut().rev().find_map(|effect| match effect {
            AppEffect::SetTerminalTextConfig(existing) => Some(existing),
            _ => None,
        }) {
            *existing = text_config;
        } else {
            effects.push(AppEffect::SetTerminalTextConfig(text_config));
        }
    }
}
pub(super) fn terminal_cursor_icon_for_mouse_shape(shape: &str) -> Option<CursorIcon> {
    let normalized = shape.to_ascii_lowercase().replace('_', "-");
    for token in normalized
        .split([';', ',', ':', '=', ' '])
        .filter(|token| !token.is_empty())
    {
        let icon = match token {
            "default" | "reset" | "arrow" => CursorIcon::Default,
            "none" | "hidden" => CursorIcon::None,
            "pointer" | "hand" | "pointing-hand" => CursorIcon::PointingHand,
            "text" | "ibeam" | "i-beam" => CursorIcon::Text,
            "vertical-text" => CursorIcon::VerticalText,
            "crosshair" => CursorIcon::Crosshair,
            "help" => CursorIcon::Help,
            "wait" => CursorIcon::Wait,
            "progress" => CursorIcon::Progress,
            "cell" => CursorIcon::Cell,
            "copy" => CursorIcon::Copy,
            "alias" => CursorIcon::Alias,
            "move" => CursorIcon::Move,
            "no-drop" => CursorIcon::NoDrop,
            "not-allowed" | "forbidden" => CursorIcon::NotAllowed,
            "grab" => CursorIcon::Grab,
            "grabbing" => CursorIcon::Grabbing,
            "all-scroll" => CursorIcon::AllScroll,
            "ew-resize" | "col-resize" | "resize-horizontal" => CursorIcon::ResizeHorizontal,
            "ns-resize" | "row-resize" | "resize-vertical" => CursorIcon::ResizeVertical,
            "nesw-resize" | "resize-nesw" => CursorIcon::ResizeNeSw,
            "nwse-resize" | "resize-nwse" => CursorIcon::ResizeNwSe,
            "e-resize" | "resize-east" => CursorIcon::ResizeEast,
            "s-resize" | "resize-south" => CursorIcon::ResizeSouth,
            "w-resize" | "resize-west" => CursorIcon::ResizeWest,
            "n-resize" | "resize-north" => CursorIcon::ResizeNorth,
            "ne-resize" | "resize-north-east" => CursorIcon::ResizeNorthEast,
            "nw-resize" | "resize-north-west" => CursorIcon::ResizeNorthWest,
            "se-resize" | "resize-south-east" => CursorIcon::ResizeSouthEast,
            "sw-resize" | "resize-south-west" => CursorIcon::ResizeSouthWest,
            "zoom-in" => CursorIcon::ZoomIn,
            "zoom-out" => CursorIcon::ZoomOut,
            _ => continue,
        };
        return Some(icon);
    }
    None
}

const fn should_toggle_native_fullscreen(window: &WindowConfig) -> bool {
    !window.uses_non_native_fullscreen_style()
}

const fn next_non_native_fullscreen_state(
    macos_handles_frame: bool,
    tracked_active: bool,
    viewport_maximized: bool,
) -> bool {
    if macos_handles_frame {
        !tracked_active
    } else {
        !viewport_maximized
    }
}
