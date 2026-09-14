use crate::gpui::{DialogIntent, SpaceEditorIntent};
use bootty_config::config::{AppearanceVariant, SshProfileConfig};
use bootty_control::Caller;
use bootty_mux::controller::SpaceId;
use bootty_mux::repository::SpaceMuxOverride;

use super::dialog_runtime::ModalDialog;
use super::{AppEffect, AppState};
use crate::commands::command_invocation_from_catalog;
use crate::input::focus::InputFocus;
use crate::presentation::dialogs::{
    CommandPaletteDialog, CommandPaletteEvent, CommandPaletteState, DialogProjection,
    DitchSessionDialog, DitchSessionEvent, KeybindHelpDialog, NewSessionDialog,
    NewSessionPickerEvent, RenameSessionDialog, RenameSessionEvent, RenameTabDialog,
    RenameTabEvent, SessionPickerDialog, SessionPickerEvent, SpaceEditorDialog, SpaceEditorEvent,
    SpacePickerDialog, SpacePickerEvent, ThemePickerDialog, ThemePickerEvent, default_space_icon,
};
use bootty_mux::workspace::{RenameSessionOutcome, ScopedSessionTarget};
impl AppState {
    /// Projects the accepted modal state without giving the renderer product ownership.
    pub fn dialog_projection(&mut self) -> Option<DialogProjection> {
        let groups = self.session_finder_groups();
        let mut dialog = self.dialogs.take()?;
        let theme_event = if let ModalDialog::ThemeEditor(editor) = dialog.as_mut() {
            editor.poll()
        } else {
            None
        };
        let pending = match dialog.as_mut() {
            ModalDialog::NewSession(dialog) => dialog.poll(),
            ModalDialog::Capture(dialog) => {
                dialog.poll();
                None
            }
            ModalDialog::SpaceEditor(dialog) => {
                dialog.poll();
                None
            }
            ModalDialog::DitchSession(dialog) => {
                dialog.poll();
                None
            }
            _ => None,
        };
        self.dialogs.replace(dialog);
        if let Some(event) = theme_event {
            self.apply_theme_editor_event(event);
        }
        if let Some(event) = pending {
            self.apply_picker_event(event);
        }
        let dialog = self.dialogs.current_mut()?;
        Some(match dialog {
            ModalDialog::NewSession(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::Capture(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::ThemeEditor(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::SpaceEditor(dialog) => {
                DialogProjection::SpaceEditor(Box::new(dialog.snapshot()))
            }
            ModalDialog::SessionPicker(dialog) => {
                dialog.update_groups(&groups);
                DialogProjection::Dialog(Box::new(dialog.spec(&groups)))
            }
            ModalDialog::RenameSession(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::RenameTab(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::DitchSession(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::KeybindHelp(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::CommandPalette(dialog) => {
                DialogProjection::Dialog(Box::new(dialog.spec()))
            }
            ModalDialog::ThemePicker(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::SpacePicker(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
        })
    }

    /// Applies an intent emitted by the generic GPUI dialog view.
    pub fn apply_dialog_intent(&mut self, intent: &DialogIntent, effects: &mut Vec<AppEffect>) {
        enum Event {
            Command(CommandPaletteEvent),
            Capture(crate::presentation::capture::CaptureEvent),
            ThemeEditor(crate::presentation::theme_editor::ThemeEditorEvent),
            Ditch(DitchSessionEvent),
            KeybindDismiss,
            NewSession(NewSessionPickerEvent),
            RenameSession(RenameSessionEvent),
            RenameTab(RenameTabEvent),
            Session(SessionPickerEvent),
            Space(SpacePickerEvent),
            Theme(ThemePickerEvent),
        }
        let groups = self.session_finder_groups();
        let open_cwds = groups
            .iter()
            .flat_map(|group| &group.sessions)
            .filter_map(|session| session.anchor.cwd.clone())
            .collect::<Vec<_>>();
        let Some(mut dialog) = self.dialogs.take() else {
            return;
        };
        let event = match dialog.as_mut() {
            ModalDialog::NewSession(dialog) => {
                dialog.apply(intent, &open_cwds).map(Event::NewSession)
            }
            ModalDialog::SpaceEditor(_) => None,
            ModalDialog::Capture(dialog) => dialog.apply(intent).map(Event::Capture),
            ModalDialog::ThemeEditor(dialog) => dialog.apply(intent).map(Event::ThemeEditor),
            ModalDialog::SessionPicker(dialog) => dialog.apply(intent).map(Event::Session),
            ModalDialog::RenameSession(dialog) => dialog.apply(intent).map(Event::RenameSession),
            ModalDialog::RenameTab(dialog) => dialog.apply(intent).map(Event::RenameTab),
            ModalDialog::DitchSession(dialog) => dialog.apply(intent).map(Event::Ditch),
            ModalDialog::KeybindHelp(dialog) => {
                dialog.apply(intent).then_some(Event::KeybindDismiss)
            }
            ModalDialog::CommandPalette(dialog) => dialog.apply(intent).map(Event::Command),
            ModalDialog::ThemePicker(dialog) => dialog.apply(intent).map(Event::Theme),
            ModalDialog::SpacePicker(dialog) => dialog.apply(intent).map(Event::Space),
        };
        self.dialogs.replace(dialog);
        match event {
            Some(Event::ThemeEditor(event)) => self.apply_theme_editor_event(event),
            Some(Event::Command(event)) => self.apply_command_palette_event(event),
            Some(Event::Capture(event)) => match event {
                crate::presentation::capture::CaptureEvent::Close => self.dismiss_modal_dialog(),
                crate::presentation::capture::CaptureEvent::Submit(command) => {
                    let cancellation = bootty_control::CommandCancellation::new();
                    let result = self.app_command_sender(Caller::Internal).submit(
                        command,
                        {
                            let now = std::time::Instant::now();
                            now.checked_add(std::time::Duration::from_secs(30))
                                .unwrap_or(now)
                        },
                        cancellation.clone(),
                    );
                    if let Some(ModalDialog::Capture(dialog)) = self.dialogs.current_mut() {
                        match result {
                            Ok(response) => dialog.started(response, cancellation),
                            Err(error) => dialog.failed(
                                match error {
                                    bootty_control::AppCommandSendError::Overloaded => {
                                        "Command queue is full; try again"
                                    }
                                    bootty_control::AppCommandSendError::Shutdown => {
                                        "Command host stopped"
                                    }
                                }
                                .to_owned(),
                            ),
                        }
                    }
                }
            },
            Some(Event::Ditch(event)) => self.apply_ditch_session_event(event),
            Some(Event::KeybindDismiss) => self.dismiss_keybind_help(),
            Some(Event::NewSession(event)) => self.apply_picker_event(event),
            Some(Event::RenameSession(event)) => self.apply_rename_session_event(event),
            Some(Event::RenameTab(event)) => self.apply_rename_tab_event(event),
            Some(Event::Session(event)) => self.apply_session_picker_event(event),
            Some(Event::Space(event)) => self.apply_space_picker_event(event),
            Some(Event::Theme(event)) => self.apply_theme_picker_event(event, effects),
            None => {}
        }
    }

    pub fn apply_space_editor_ui_intent(&mut self, intent: SpaceEditorIntent) {
        let Some(ModalDialog::SpaceEditor(dialog)) = self.dialogs.current_mut() else {
            return;
        };
        if let Some(event) = dialog.apply(intent) {
            self.apply_space_editor_event(event);
        }
    }

    fn show_overlay(&mut self, dialog: ModalDialog) {
        self.dialogs.open(dialog);
    }

    fn open_overlay(&mut self, dialog: ModalDialog) {
        self.close_overlay_dialogs();
        self.show_overlay(dialog);
    }

    fn ssh_profiles(&self) -> Vec<(String, SshProfileConfig)> {
        self.config()
            .ssh_profiles
            .iter()
            .map(|(id, profile)| (id.clone(), profile.clone()))
            .collect()
    }

    fn selected_session_id(&self) -> Option<String> {
        self.workspace
            .active
            .binding
            .mux()
            .selected_session()
            .map(str::to_owned)
    }

    pub fn modal_dialog(&self) -> Option<&ModalDialog> {
        self.dialogs.current()
    }

    pub fn modal_dialog_mut(&mut self) -> Option<&mut ModalDialog> {
        self.dialogs.current_mut()
    }

    fn dismiss_modal_dialog(&mut self) {
        self.dialogs.clear();
    }
    pub fn apply_space_editor_event(&mut self, intent: SpaceEditorEvent) {
        match intent {
            SpaceEditorEvent::Close => self.dismiss_modal_dialog(),
            SpaceEditorEvent::Save(draft) => {
                let mux = SpaceMuxOverride {
                    backend: draft.backend,
                    remote: draft.remote_source,
                };
                let saved = match draft.space_id {
                    Some(space_id) => self.update_space_from_ui(
                        space_id,
                        &draft.name,
                        &draft.icon,
                        draft.color,
                        draft.tint_sidebar,
                        mux,
                    ),
                    None => self.create_space_with_backend_from_ui(
                        &draft.name,
                        &draft.icon,
                        draft.color,
                        draft.tint_sidebar,
                        mux,
                    ),
                };
                if saved {
                    self.dismiss_modal_dialog();
                }
            }
        }
    }
    pub fn apply_space_picker_event(&mut self, event: SpacePickerEvent) {
        match event {
            SpacePickerEvent::Close => self.dismiss_modal_dialog(),
            SpacePickerEvent::Move { session, space } => {
                let moved = match space {
                    Some(space) => self.move_scoped_session_to_space(&session, space),
                    None => self.detach_scoped_session_from_space(&session),
                };
                if moved {
                    self.dismiss_modal_dialog();
                }
            }
        }
    }

    /// Opens the Space picker for a session, or reports why it cannot move.
    pub fn open_space_picker_for(&mut self, target: &ScopedSessionTarget) -> bool {
        let Some(name) = self.session_display_name(target) else {
            self.record_notice(crate::error_catalog::ErrorNotice::SessionUnavailable);
            return false;
        };
        let spaces = self.session_move_targets(target);
        if spaces.iter().all(|space| space.current) {
            self.record_notice(crate::error_catalog::ErrorNotice::NoSpaceToMoveSession);
            return false;
        }
        self.open_overlay(ModalDialog::SpacePicker(SpacePickerDialog::open(
            target.clone(),
            name,
            spaces,
        )));
        true
    }

    pub fn apply_session_picker_event(&mut self, event: SessionPickerEvent) {
        match event {
            SessionPickerEvent::Close => self.dismiss_modal_dialog(),
            SessionPickerEvent::ActivateSession(target) => {
                self.dismiss_modal_dialog();
                if let Err(error) = self.workspace.adopt_session_into_binding(
                    target.scope,
                    &target.session_id,
                    &self.repaint,
                ) {
                    self.record_error(error);
                    return;
                }
                self.activate_scoped_session_from_ui(&target);
            }
        }
    }
    pub fn apply_rename_session_event(&mut self, event: RenameSessionEvent) {
        match event {
            RenameSessionEvent::Close => self.dismiss_modal_dialog(),
            RenameSessionEvent::Rename { session_id, name } => {
                let name = name.trim().to_owned();
                if name.is_empty() {
                    self.record_notice(crate::error_catalog::ErrorNotice::SessionNameEmpty);
                    return;
                }
                match self
                    .workspace
                    .rename_active_session(&session_id, &name, &self.repaint)
                {
                    Ok(RenameSessionOutcome::Missing | RenameSessionOutcome::Started) => {}
                    Ok(RenameSessionOutcome::Pending) => return,
                    Err(error) => {
                        self.record_error(error);
                        return;
                    }
                }
                self.dismiss_modal_dialog();
            }
        }
    }
    pub fn apply_rename_tab_event(&mut self, event: RenameTabEvent) {
        match event {
            RenameTabEvent::Close => self.dismiss_modal_dialog(),
            RenameTabEvent::Rename {
                session_id,
                window_id,
                name,
            } => {
                let name = name.trim();
                self.workspace.active.binding.set_custom_window_name(
                    &session_id,
                    &window_id,
                    name,
                    &self.repaint,
                );
                self.dismiss_modal_dialog();
            }
        }
    }
    pub fn apply_ditch_session_event(&mut self, event: DitchSessionEvent) {
        match event {
            DitchSessionEvent::Close => self.dismiss_modal_dialog(),
            DitchSessionEvent::Ditch {
                session_id,
                cwd,
                action,
            } => {
                if self.active_multiplexer().remote.is_some()
                    && !matches!(action, crate::presentation::dialogs::DitchAction::KillOnly)
                {
                    self.record_notice(crate::error_catalog::ErrorNotice::Ditch(
                        "Remote worktree cleanup is not supported; close the session without cleanup".to_owned(),
                    ));
                    return;
                }
                let Ok(prepared) = self.prepare_ditch_session_command(session_id) else {
                    return;
                };
                if matches!(action, crate::presentation::dialogs::DitchAction::KillOnly) {
                    self.submit_prepared_ditch_session_command(prepared);
                } else {
                    self.submit_ditch_cleanup(prepared, cwd, &action);
                }
                self.dismiss_modal_dialog();
            }
        }
    }
    pub fn dismiss_keybind_help(&mut self) {
        self.dismiss_modal_dialog();
    }
    pub fn apply_command_palette_event(&mut self, event: CommandPaletteEvent) {
        match event {
            CommandPaletteEvent::Close => self.dismiss_modal_dialog(),
            CommandPaletteEvent::Run(command) => {
                // Resolve the user's current context before another queued caller can change it.
                self.dismiss_modal_dialog();
                let Some(mut invocation) =
                    command_invocation_from_catalog(command, Caller::CommandPalette)
                else {
                    return;
                };
                if let Some(kind) = self.commands.target_kind(&invocation.command) {
                    let Some(target) = self.current_command_target_for(&invocation.command, kind)
                    else {
                        self.commands.clear_queue();
                        self.record_notice(crate::error_catalog::ErrorNotice::NoCurrentTarget(
                            format!("no current {kind:?} target is available"),
                        ));
                        return;
                    };
                    invocation.target = Some(target);
                }
                self.commands.queue(invocation);
            }
        }
    }
    pub fn apply_theme_picker_event(
        &mut self,
        event: ThemePickerEvent,
        effects: &mut Vec<AppEffect>,
    ) {
        match event {
            ThemePickerEvent::Close => {
                self.dismiss_modal_dialog();
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
                self.theme_picker_restore_config = None;
            }
            ThemePickerEvent::RestorePreview => {
                if self.restore_theme_picker_preview() {
                    effects.push(AppEffect::RequestRepaint);
                }
            }
            ThemePickerEvent::Preview(theme) => {
                self.preview_active_theme(&theme, effects);
            }
            ThemePickerEvent::Select(theme) => {
                self.dismiss_modal_dialog();
                self.theme_picker_restore_config = None;
                self.persist_active_theme(&theme, effects);
            }
        }
    }
    pub(crate) fn open_theme_editor(&mut self) {
        self.close_overlay_dialogs();
        let name = self
            .config()
            .theme_for_appearance(self.active_appearance_variant)
            .unwrap_or("Bootty")
            .to_owned();
        let editor = crate::presentation::theme_editor::ThemeEditorDialog::new(
            name,
            self.active_appearance_variant,
        );
        let event = editor.load();
        self.show_overlay(ModalDialog::ThemeEditor(editor));
        self.apply_theme_editor_event(event);
    }

    fn apply_theme_editor_event(
        &mut self,
        event: crate::presentation::theme_editor::ThemeEditorEvent,
    ) {
        use crate::presentation::theme_editor::ThemeEditorEvent;
        match event {
            ThemeEditorEvent::Close => {
                self.close_overlay_dialogs();
            }
            ThemeEditorEvent::Submit(command) => {
                let action = command.command.clone();
                let cancellation = bootty_control::CommandCancellation::new();
                let result = self.app_command_sender(Caller::Internal).submit(
                    command,
                    {
                        let now = std::time::Instant::now();
                        now.checked_add(std::time::Duration::from_secs(30))
                            .unwrap_or(now)
                    },
                    cancellation.clone(),
                );
                if let Some(ModalDialog::ThemeEditor(editor)) = self.dialogs.current_mut() {
                    match result {
                        Ok(receiver) => editor.started(action, receiver, cancellation),
                        Err(error) => {
                            editor.failed(format!("Theme command could not be queued: {error:?}"));
                        }
                    }
                }
            }
        }
    }

    pub(crate) fn open_capture_dialog(&mut self) {
        let Some(target) = self
            .current_command_target_for("terminal.export", bootty_control::ResourceKind::Terminal)
        else {
            self.record_error("Export needs an attached terminal");
            return;
        };
        let destination = bootty_git::home_dir()
            .unwrap_or_default()
            .join("Downloads/bootty-terminal.txt")
            .to_string_lossy()
            .into_owned();
        self.show_overlay(ModalDialog::Capture(
            crate::presentation::capture::CaptureDialog::new(target, destination),
        ));
    }

    pub fn apply_picker_event(&mut self, event: NewSessionPickerEvent) {
        match event {
            NewSessionPickerEvent::Close => self.dismiss_modal_dialog(),
            NewSessionPickerEvent::Error(error) => {
                self.record_error(error);
            }
            NewSessionPickerEvent::CreateWorktree { repo, request } => {
                match bootty_git::Git::new().create_worktree(&repo, &request) {
                    Ok(path) => {
                        self.create_project_session_for_cwd(&path);
                        self.dismiss_modal_dialog();
                    }
                    Err(error) => {
                        self.record_notice(crate::error_catalog::ErrorNotice::Worktree(format!(
                            "worktree: {error}"
                        )));
                    }
                }
            }
            NewSessionPickerEvent::CreateSession { cwd } => {
                self.create_project_session_for_cwd(&cwd);
                self.dismiss_modal_dialog();
            }
        }
    }
    pub(crate) fn close_overlay_dialogs(&mut self) -> bool {
        let restored_preview = self.restore_theme_picker_preview();
        self.theme_picker_restore_config = None;
        self.dialogs.clear();
        if self.input_focus == InputFocus::Find {
            self.input_focus = InputFocus::Terminal;
        }
        let outcome = self
            .terminal_interaction
            .close_overlay_dialogs(self.workspace.active.binding.terminal_mut());
        self.apply_terminal_outcome(outcome.last_error, outcome.focus_intent);
        restored_preview
    }
    pub(super) fn open_new_mux_session_dialog(&mut self) {
        self.close_overlay_dialogs();
        self.show_overlay(ModalDialog::NewSession(
            self.active_multiplexer().remote.clone().map_or_else(
                || NewSessionDialog::open_local(self.repaint.clone()),
                |remote| NewSessionDialog::open_remote(remote, self.repaint.clone()),
            ),
        ));
    }
    pub fn open_create_space_dialog_from_ui(&mut self) -> bool {
        self.close_overlay_dialogs();
        let existing_icons = self
            .space_summaries()
            .into_iter()
            .map(|space| space.icon)
            .collect::<Vec<_>>();
        let profiles = self.ssh_profiles();
        self.show_overlay(ModalDialog::SpaceEditor(
            SpaceEditorDialog::new_space(
                default_space_icon(&existing_icons),
                SpaceMuxOverride::default(),
            )
            .with_profiles(profiles.into_iter())
            .discover_wsl(self.repaint.clone()),
        ));
        true
    }
    pub fn open_edit_space_dialog_from_ui(&mut self, space_id: SpaceId) -> bool {
        let placement = self.workspace.space_placement(space_id);
        let Some((space, placement)) = self
            .space_summaries()
            .into_iter()
            .find(|space| space.id == space_id)
            .zip(placement)
        else {
            return false;
        };
        self.close_overlay_dialogs();
        let profiles = self.ssh_profiles();
        self.show_overlay(ModalDialog::SpaceEditor(
            SpaceEditorDialog::edit_space(
                space.id,
                space.name,
                space.icon,
                space.color,
                space.tint_sidebar,
                placement,
            )
            .with_profiles(profiles.into_iter())
            .discover_wsl(self.repaint.clone()),
        ));
        true
    }
    pub fn open_new_session_dialog_from_ui(&mut self) -> bool {
        self.open_new_mux_session_dialog();
        true
    }
    pub(super) fn open_session_picker_dialog(&mut self) {
        self.open_overlay(ModalDialog::SessionPicker(SessionPickerDialog::open()));
    }
    pub fn open_session_picker_dialog_from_ui(&mut self) -> bool {
        self.open_session_picker_dialog();
        true
    }
    pub(super) fn toggle_session_picker_dialog(&mut self) {
        if self.dialogs.is_session_picker() {
            self.dialogs.clear();
        } else {
            self.open_session_picker_dialog();
        }
    }
    pub(super) fn open_space_picker_for_current_session(&mut self) -> bool {
        let Some(selected) = self.selected_session_id() else {
            return false;
        };
        let target = ScopedSessionTarget::new(self.workspace.active.binding.scope(), selected);
        self.open_space_picker_for(&target)
    }

    pub(super) fn open_rename_session_dialog(&mut self) {
        let Some(selected) = self.selected_session_id() else {
            return;
        };
        self.open_rename_session_dialog_for(&selected);
    }
    pub fn open_rename_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, name)) = self
            .workspace
            .active
            .binding
            .mux()
            .session_by_id_or_name(session_id)
            .map(|session| {
                // Prefill what bootty shows, so a backend-only uniqueness suffix is not something
                // the user has to delete out of the field.
                let name = session
                    .tag
                    .identity
                    .as_deref()
                    .and_then(|identity| self.workspace.active.binding.sessions().get(identity))
                    .map_or(session.name.as_str(), |claimed| claimed.label())
                    .to_owned();
                (session.id.clone(), name)
            })
        else {
            return false;
        };
        self.open_overlay(ModalDialog::RenameSession(RenameSessionDialog::open(
            session_id, name,
        )));
        true
    }
    pub(super) fn open_rename_tab_dialog(&mut self) {
        let Some((session_id, window_id, _)) = self.selected_window_for_rename() else {
            return;
        };
        self.open_rename_tab_dialog_for(&session_id, &window_id);
    }
    pub fn open_rename_tab_dialog_for(&mut self, session_id: &str, window_id: &str) -> bool {
        let Some((session_id, window_id, name)) = self
            .workspace
            .active
            .binding
            .mux()
            .session_by_id_or_name(session_id)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|window| window.id == window_id)
                    .map(|window| (session.id.clone(), window.id.clone(), window.name.clone()))
            })
        else {
            return false;
        };
        self.open_overlay(ModalDialog::RenameTab(RenameTabDialog::open(
            session_id, window_id, name,
        )));
        true
    }
    fn selected_window_for_rename(&self) -> Option<(String, String, String)> {
        let selected = self.workspace.active.binding.mux().selected_session()?;
        let session = self
            .workspace
            .active
            .binding
            .mux()
            .session_by_id_or_name(selected)?;
        let window_id = self
            .workspace
            .active
            .binding
            .mux()
            .selected_window()
            .or(session.active_window_id.as_deref());
        let window = window_id
            .and_then(|id| session.windows.iter().find(|window| window.id == id))
            .or_else(|| session.windows.first())?;
        Some((session.id.clone(), window.id.clone(), window.name.clone()))
    }
    pub(super) fn open_ditch_session_dialog(&mut self) {
        let Some(selected) = self.selected_session_id() else {
            return;
        };
        self.open_ditch_session_dialog_for(&selected);
    }
    pub fn open_ditch_session_dialog_for(&mut self, session_id: &str) -> bool {
        let Some((session_id, cwd)) = self
            .workspace
            .active
            .binding
            .mux()
            .session_by_id_or_name(session_id)
            .map(|session| (session.id.clone(), session.anchor.cwd.clone()))
        else {
            return false;
        };
        let dialog = if self.active_multiplexer().remote.is_some() {
            DitchSessionDialog::open_remote(session_id, cwd)
        } else {
            DitchSessionDialog::open_with_repaint(session_id, cwd, self.repaint.clone())
        };
        self.open_overlay(ModalDialog::DitchSession(dialog));
        true
    }
    pub(super) fn open_keybind_help_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer().backend);
        self.open_overlay(ModalDialog::KeybindHelp(KeybindHelpDialog::open(&bindings)));
    }
    pub(super) fn open_command_palette_dialog(&mut self) {
        let bindings = self
            .config()
            .input
            .keybinds_for_backend(self.workspace.active.binding.multiplexer().backend);
        self.open_overlay(ModalDialog::CommandPalette(
            CommandPaletteDialog::open_localized(
                &bindings,
                CommandPaletteState {
                    appearance_mode: self.config().appearance.mode,
                    sidebar_visible: self.config().chrome.sidebar,
                },
                &self.localizer,
            ),
        ));
    }
    pub(super) fn open_theme_picker_dialog(&mut self) {
        let config = self.config();
        let branch = match self.active_appearance_variant {
            AppearanceVariant::Light => "Light appearance",
            AppearanceVariant::Dark => "Dark appearance",
        };
        let current = config
            .theme_for_appearance(self.active_appearance_variant)
            .map(str::to_owned);
        let names = bootty_config::config::available_theme_names(&config.config_path);
        let restore_config = config.clone();
        self.close_overlay_dialogs();
        self.theme_picker_restore_config = Some(restore_config);
        self.show_overlay(ModalDialog::ThemePicker(ThemePickerDialog::open(
            names,
            current,
            branch.to_owned(),
        )));
    }
}
