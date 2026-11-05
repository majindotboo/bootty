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
    KeybindHelpDialog, NewSessionDialog, NewSessionPickerEvent, RenameSessionDialog,
    RenameSessionEvent, RenameTabDialog, RenameTabEvent, SessionPickerDialog, SessionPickerEvent,
    SpaceEditorDialog, SpaceEditorEvent, SpacePickerDialog, SpacePickerEvent, ThemePickerDialog,
    ThemePickerEvent, default_space_icon,
};
use bootty_mux::workspace::ScopedSessionTarget;
impl AppState {
    pub(crate) fn open_project_settings(
        &mut self,
        project: crate::presentation::project_editor::ProjectSettingsEditor,
    ) {
        self.dialogs.project_settings = Some(project);
    }
    pub(crate) const fn take_project_settings(
        &mut self,
    ) -> Option<crate::presentation::project_editor::ProjectSettingsEditor> {
        self.dialogs.project_settings.take()
    }
    /// Projects the accepted modal state without giving the renderer product ownership.
    pub fn dialog_projection(&mut self) -> Option<DialogProjection> {
        self.poll_accepted_creations();
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
            _ => None,
        };
        self.dialogs.replace(dialog);
        if let Some(event) = theme_event {
            self.apply_theme_editor_event(event);
        }
        if let Some(event) = pending {
            self.apply_picker_event(event);
        }
        if let Some(ModalDialog::NewSession(dialog)) = self.dialogs.current_mut()
            && let Some(draft) = dialog.draft()
        {
            let projects = self
                .workspace
                .registered_projects(draft.scope)
                .map(|project| bootty_git::ProjectPickerEntry {
                    path: project.cwd.clone(),
                    favorite: false,
                })
                .collect();
            dialog.sync_registered_projects(projects);
        }
        let names = matches!(self.dialogs.current(), Some(ModalDialog::SessionPicker(_)))
            .then(|| self.saved_session_names())
            .unwrap_or_default();
        let dialog = self.dialogs.current_mut()?;
        Some(match dialog {
            ModalDialog::NewSession(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::Capture(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::ThemeEditor(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::SpaceEditor(dialog) => {
                DialogProjection::SpaceEditor(Box::new(dialog.snapshot()))
            }
            ModalDialog::SessionPicker(dialog) => {
                dialog.update_groups(&groups, &names);
                DialogProjection::Dialog(Box::new(dialog.spec(&groups)))
            }
            ModalDialog::RenameSession(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::RenameTab(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::KeybindHelp(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::CommandPalette(dialog) => {
                DialogProjection::Dialog(Box::new(dialog.spec()))
            }
            ModalDialog::ThemePicker(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
            ModalDialog::SpacePicker(dialog) => DialogProjection::Dialog(Box::new(dialog.spec())),
        })
    }

    fn poll_accepted_creations(&mut self) {
        let mut messages = Vec::new();
        self.dialogs
            .creation_replies
            .retain(|reply| match reply.try_recv() {
                Ok(bootty_control::CommandOutcome::Success { warnings, .. }) => {
                    messages.extend(
                        warnings
                            .into_iter()
                            .map(|warning| ("The session started with a problem", warning.message)),
                    );
                    false
                }
                Ok(outcome) => {
                    messages.extend(
                        crate::commands::command_outcome_message(&outcome)
                            .map(|message| ("Could not start the session", message)),
                    );
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    messages.push((
                        "Could not start the session",
                        "Session startup owner stopped".to_owned(),
                    ));
                    false
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => true,
            });
        for (action, message) in messages {
            self.record_error(action, &message);
        }
    }

    pub(crate) fn creation_underlay(&self) -> Option<crate::gpui::DialogSpec> {
        self.dialogs.creation_spec()
    }

    /// Applies an intent emitted by the generic GPUI dialog view.
    pub fn apply_dialog_intent(&mut self, intent: &DialogIntent, effects: &mut Vec<AppEffect>) {
        enum Event {
            Command(CommandPaletteEvent),
            Capture(crate::presentation::capture::CaptureEvent),
            ThemeEditor(crate::presentation::theme_editor::ThemeEditorEvent),
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

    pub(crate) fn open_overlay(&mut self, dialog: ModalDialog) {
        if !matches!(self.dialogs.current(), Some(ModalDialog::NewSession(_))) {
            self.close_overlay_dialogs();
        }
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

    pub(super) fn dismiss_modal_dialog(&mut self) {
        self.dialogs.clear();
    }

    pub fn apply_space_editor_event(&mut self, intent: SpaceEditorEvent) {
        match intent {
            SpaceEditorEvent::Close => self.dismiss_modal_dialog(),
            SpaceEditorEvent::Save(draft) => {
                let mux = SpaceMuxOverride {
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
                    self.record_error("Could not move the session", &error);
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
                let scope = self.mux_scope();
                let Some(identity) = self.workspace.session_identity(scope, &session_id) else {
                    return;
                };
                let Some(invocation) =
                    self.saved_session_invocation(scope, "session.set_title", vec![identity, name])
                else {
                    return;
                };
                let outcome = self.dispatch_command(
                    invocation,
                    super::ViewportSnapshot::default(),
                    &mut Vec::new(),
                );
                if !matches!(outcome, bootty_control::CommandOutcome::Success { .. }) {
                    return;
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
    pub fn dismiss_keybind_help(&mut self) {
        self.dismiss_modal_dialog();
    }
    pub fn apply_command_palette_event(&mut self, event: CommandPaletteEvent) {
        let invocation = match event {
            CommandPaletteEvent::Close => {
                self.dismiss_modal_dialog();
                return;
            }
            CommandPaletteEvent::Run(command) => {
                command_invocation_from_catalog(command, Caller::CommandPalette)
            }
            CommandPaletteEvent::Invoke(command) => Some(bootty_control::CommandInvocation::new(
                command,
                Vec::new(),
                Caller::CommandPalette,
            )),
        };
        self.dismiss_modal_dialog();
        let Some(mut invocation) = invocation else {
            return;
        };
        if invocation.target.is_none()
            && let Some(kind) = self.commands.target_kind(&invocation.command)
        {
            let Some(target) = self.current_command_target_for(&invocation.command, kind) else {
                self.commands.clear_queue();
                self.record_notice(crate::error_catalog::ErrorNotice::NoCurrentTarget(format!(
                    "no current {kind:?} target is available"
                )));
                return;
            };
            invocation.target = Some(target);
        }
        self.commands.queue(invocation);
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
            self.record_message("Export needs an attached terminal");
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

    /// Accepted creation owns removal of its chooser and draft; this is not cancellation.
    fn complete_creation(&mut self) {
        let identity = match self.dialogs.current() {
            Some(ModalDialog::NewSession(dialog)) => {
                dialog.draft().map(|draft| draft.identity.clone())
            }
            _ => None,
        };
        self.dialogs.clear();
        if let Some(identity) = identity {
            self.dialogs.remove_creation_draft(&identity);
        }
    }

    fn session_started(
        &mut self,
        value: &serde_json::Value,
        warnings: Vec<bootty_control::CommandWarning>,
        setup: Option<String>,
    ) {
        if let Some(repository) = setup {
            self.run_setup_scripts(value, repository);
        }
        // The new session becomes the selected one, as a new tab does.
        if let Some(Err(outcome)) = value
            .get("terminal")
            .and_then(|terminal| {
                serde_json::from_value::<bootty_control::CommandTarget>(terminal.clone()).ok()
            })
            .map(|target| self.activate_created_terminal(&target))
        {
            self.record_error(
                "Could not select the new session",
                &crate::commands::command_outcome_message(&outcome).unwrap_or_default(),
            );
        }
        if !warnings.is_empty() {
            self.record_error(
                "The session started with a problem",
                &warnings
                    .into_iter()
                    .map(|warning| warning.message)
                    .collect::<Vec<_>>()
                    .join("; "),
            );
        }
        self.complete_creation();
    }

    pub fn apply_picker_event(&mut self, event: NewSessionPickerEvent) {
        match event {
            NewSessionPickerEvent::Branches(invocation) => self.request_branches(invocation),
            NewSessionPickerEvent::CheckoutBranch(invocation) => {
                let now = std::time::Instant::now();
                let result = self.app_command_sender(Caller::Internal).submit(
                    invocation.clone(),
                    now.checked_add(std::time::Duration::from_secs(30))
                        .unwrap_or(now),
                    bootty_control::CommandCancellation::new(),
                );
                if let Some(ModalDialog::NewSession(dialog)) = self.dialogs.current_mut() {
                    match result {
                        Ok(receiver) => dialog.checkout_started(invocation, receiver),
                        Err(error) => dialog.checkout_failed(&invocation, &format!("{error:?}")),
                    }
                }
            }
            NewSessionPickerEvent::Submit(invocation) => {
                let cancellation = bootty_control::CommandCancellation::new();
                let now = std::time::Instant::now();
                let result = self.app_command_sender(Caller::Internal).submit(
                    invocation,
                    now.checked_add(std::time::Duration::from_secs(30))
                        .unwrap_or(now),
                    cancellation,
                );
                if let Some(ModalDialog::NewSession(dialog)) = self.dialogs.current_mut() {
                    match result {
                        Ok(receiver) => dialog.started(receiver),
                        Err(error) => dialog.failed(format!("Session could not start: {error:?}")),
                    }
                }
            }
            NewSessionPickerEvent::Started {
                value,
                warnings,
                setup,
            } => self.session_started(&value, warnings, setup),
            NewSessionPickerEvent::Close => {
                self.dismiss_modal_dialog();
            }
            NewSessionPickerEvent::ProjectAdded { path } => self.register_picked_project(path),
            NewSessionPickerEvent::AddProject { path } => {
                let result = self.dialogs.current_mut().and_then(|dialog| match dialog {
                    ModalDialog::NewSession(dialog) => Some(dialog.start_add_project(path)),
                    _ => None,
                });
                match result {
                    Some(Ok(())) => {}
                    Some(Err(error)) => self.record_error("Could not add the project", &error),
                    None => self.record_message("The project picker is no longer open"),
                }
            }
            NewSessionPickerEvent::Error(error) => {
                self.record_error("Could not start the session", &error);
            }
            NewSessionPickerEvent::CreateWorktree { repo, request } => {
                self.create_picked_worktree(repo, request);
            }
            NewSessionPickerEvent::CreateSession { cwd } => {
                let bootty_mux::command::MuxCommand::CreateProjectSession {
                    session_id, cwd, ..
                } = self.workspace.project_session_command(&cwd)
                else {
                    return;
                };
                let title = self.workspace.project_session_title(&cwd);
                if !matches!(self.dialogs.current(), Some(ModalDialog::NewSession(_))) {
                    self.open_new_mux_session_dialog();
                }
                if let Some(invocation) = self.saved_session_invocation(
                    self.mux_scope(),
                    "session.create",
                    vec![
                        session_id,
                        cwd,
                        "[]".to_owned(),
                        bootty_mux::snapshot::new_session_identity(),
                        title,
                    ],
                ) {
                    self.apply_picker_event(NewSessionPickerEvent::Submit(invocation));
                }
            }
        }
    }

    fn create_picked_worktree(&mut self, repo: String, request: bootty_git::WorktreeRequest) {
        let Some(prepared) = self.dialogs.current_mut().and_then(|dialog| match dialog {
            ModalDialog::NewSession(dialog) => Some(dialog.start_create_worktree(repo, request)),
            _ => None,
        }) else {
            return;
        };
        let result = prepared.and_then(|invocation| {
            let now = std::time::Instant::now();
            self.app_command_sender(Caller::Internal)
                .submit(
                    invocation,
                    now.checked_add(std::time::Duration::from_secs(120))
                        .unwrap_or(now),
                    bootty_control::CommandCancellation::new(),
                )
                .map_err(|error| format!("Worktree creation could not start: {error:?}"))
        });
        if let Some(ModalDialog::NewSession(dialog)) = self.dialogs.current_mut() {
            match result {
                Ok(reply) => dialog.worktree_started(reply),
                Err(error) => dialog.failed(error),
            }
        }
    }

    fn register_picked_project(&mut self, path: String) {
        let target = match self.dialogs.current() {
            Some(ModalDialog::NewSession(dialog)) => dialog.registration_target(),
            _ => None,
        };
        let Some(target) = target else {
            return;
        };
        let mut invocation = bootty_control::CommandInvocation::new(
            "project.register",
            vec![path],
            Caller::Internal,
        );
        invocation.target = Some(target);
        let outcome = self.dispatch_command(
            invocation,
            super::ViewportSnapshot::default(),
            &mut Vec::new(),
        );
        if matches!(outcome, bootty_control::CommandOutcome::Success { .. }) {
            self.dismiss_modal_dialog();
        }
    }

    fn request_branches(&mut self, invocation: bootty_control::CommandInvocation) {
        let now = std::time::Instant::now();
        let result = self.app_command_sender(Caller::Internal).submit(
            invocation.clone(),
            now.checked_add(std::time::Duration::from_secs(30))
                .unwrap_or(now),
            bootty_control::CommandCancellation::new(),
        );
        if let Some(ModalDialog::NewSession(dialog)) = self.dialogs.current_mut() {
            match result {
                Ok(receiver) => dialog.branches_started(invocation, receiver),
                Err(error) => dialog.branches_failed(
                    &invocation,
                    format!("The branch catalog is unavailable: {error:?}"),
                ),
            }
        }
    }

    /// Runs the project's setup scripts in the session that just started in a new worktree of
    /// `repository`, each in its own tab.
    fn run_setup_scripts(&mut self, value: &serde_json::Value, repository: String) {
        let Some(terminal) = value
            .get("terminal")
            .and_then(|terminal| serde_json::from_value(terminal.clone()).ok())
        else {
            return;
        };
        let mut setup = bootty_control::CommandInvocation::new(
            "project.setup",
            vec![repository],
            Caller::Internal,
        );
        setup.target = Some(terminal);
        self.commands.queue(setup);
    }

    pub(crate) fn modal_dialog_dismissible(&self) -> bool {
        self.dialogs.is_dismissible()
    }

    pub(crate) fn dismiss_session_creation(&mut self) {
        if matches!(self.dialogs.current(), Some(ModalDialog::NewSession(_))) {
            self.close_overlay_dialogs();
        }
    }

    pub(crate) fn close_overlay_dialogs(&mut self) -> bool {
        if !self.dialogs.is_dismissible() {
            return false;
        }
        let restored_preview = self.restore_theme_picker_preview();
        self.theme_picker_restore_config = None;
        self.dismiss_modal_dialog();
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
        self.open_session_creation_dialog(None, None);
    }
    pub(crate) fn show_creation_for_empty_workspace(&mut self) {
        if let Some(identity) = self.dialogs.restore_creation.take()
            && let Some(record) = self.dialogs.creation_drafts.get(&identity)
        {
            let scope = record.draft.scope;
            if let Err(error) = self.open_creation_draft(scope, &identity) {
                self.record_error("Could not restore the session draft", &error);
            }
            return;
        }
        let binding = &self.workspace.active.binding;
        if !binding.member_sessions().is_empty() {
            self.dialogs.empty_creation_scope = None;
            if self.dialogs.automatic_creation && self.dialogs.active_creation_identity().is_none()
            {
                self.dismiss_session_creation();
            }
            return;
        }
        let scope = binding.scope();
        if !self.dialogs.has_modal()
            && self.dialogs.empty_creation_scope != Some(scope)
            && binding.mux().has_session_snapshot()
            && binding.mux().unavailable_reason().is_none()
        {
            self.dialogs.empty_creation_scope = Some(scope);
            self.open_session_creation_dialog(None, None);
            self.dialogs.automatic_creation = true;
        }
    }
    /// Opens the new-session form in a Space's directory, such as a session's project.
    pub fn open_new_session_dialog_in(&mut self, scope: SpaceId, cwd: String) -> bool {
        self.open_session_creation_dialog(Some((scope, cwd)), None);
        true
    }
    pub(crate) fn open_new_session_on_branch(&mut self, scope: SpaceId, cwd: String) -> bool {
        self.open_new_session_dialog_in(scope, cwd);
        self.apply_dialog_intent(
            &DialogIntent::FieldChanged {
                dialog: crate::gpui::DialogId::new(crate::presentation::dialogs::NEW_SESSION_ID),
                field: "isolation".into(),
                value: "Current checkout".into(),
            },
            &mut Vec::new(),
        );
        true
    }
    pub(crate) fn open_creation_draft(
        &mut self,
        scope: SpaceId,
        identity: &str,
    ) -> Result<(), String> {
        let (records, _) = self.dialogs.creation_snapshot();
        let record = records
            .get(identity)
            .filter(|record| record.draft.scope == scope)
            .cloned()
            .ok_or("This draft is no longer available in this Space")?;
        if self
            .workspace
            .binding(scope)
            .and_then(|binding| binding.sessions().get(identity))
            .is_some_and(|saved| saved.terminal_snapshot.is_some())
            && let Some(invocation) =
                self.saved_session_invocation(scope, "session.reopen", vec![identity.to_owned()])
        {
            self.commands.queue(invocation);
            return Ok(());
        }
        if scope != self.mux_scope() && !self.activate_space_from_ui(scope) {
            return Err("The draft's Space could not be opened; the draft is still saved".into());
        }
        let place = Some((scope, record.draft.cwd.clone()));
        self.open_session_creation_dialog(place, Some(record));
        Ok(())
    }

    fn open_session_creation_dialog(
        &mut self,
        place: Option<(SpaceId, String)>,
        retained: Option<super::creation_draft::CreationDraft>,
    ) {
        use crate::presentation::new_session_form::NewSessionForm;
        self.close_overlay_dialogs();
        self.dialogs.automatic_creation = false;
        let mut destinations = self.new_session_destinations();
        let current = if let Some((scope, cwd)) = place {
            let Some(destination) = destinations
                .iter_mut()
                .find(|destination| destination.scope == scope)
            else {
                self.record_message("That Space can't create a session");
                return;
            };
            destination.cwd = cwd;
            scope
        } else {
            self.mux_scope()
        };
        let Some(destination) = destinations
            .iter()
            .find(|destination| destination.scope == current)
        else {
            self.record_message("The current host cannot create a session");
            return;
        };
        let draft = retained.as_ref().map_or_else(
            || self.session_creation_draft(destination),
            |record| record.draft.clone(),
        );
        let mut form = NewSessionForm::new(draft, destinations);
        if let Some(record) = retained {
            form.set_project_path(record.project);
        }
        form.set_worktree_directory(self.config().session.worktree_directory.clone());
        form.set_branch_prefix(self.config().session.branch_prefix.clone());
        form.set_project_defaults(
            self.workspace
                .spaces()
                .flat_map(|space| self.workspace.registered_projects(space.id).cloned())
                .collect(),
        );
        let dialog = NewSessionDialog::open_registered_form(
            form,
            &self.repaint,
            self.workspace.project_repository(),
        );
        self.show_overlay(ModalDialog::NewSession(Box::new(dialog)));
    }
    fn session_creation_draft(
        &self,
        destination: &crate::presentation::new_session_form::SessionDestination,
    ) -> crate::presentation::new_session_form::NewSessionDraft {
        use crate::presentation::new_session_form::NewSessionDraft;
        let suffix = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |duration| duration.as_nanos());
        NewSessionDraft {
            scope: destination.scope,
            cwd: destination.cwd.clone(),
            projectless: false,
            isolated: self.dialogs.new_session_isolated,
            isolation_preference: self.dialogs.new_session_isolated,
            branch: String::new(),
            folder: String::new(),
            start_ref: String::new(),
            suffix: format!("{suffix:x}"),
            identity: bootty_mux::snapshot::new_session_identity(),
            directories: std::collections::HashMap::new(),
        }
    }
    fn new_session_destinations(
        &self,
    ) -> Vec<crate::presentation::new_session_form::SessionDestination> {
        use crate::presentation::new_session_form::SessionDestination;
        let spaces = self.space_summaries();
        self.workspace
            .all_bindings()
            .filter_map(|binding| {
                let scope = binding.scope();
                let mux = binding.mux();
                let handle = self.binding_target_handle(scope, mux.binding_generation());
                let target = crate::commands::ExactMuxTarget::Binding(scope).command_target(
                    bootty_control::ResourceKind::Binding,
                    mux,
                    &handle,
                )?;
                let remote = binding.multiplexer().remote.clone();
                let cwd = mux
                    .selected_session()
                    .and_then(|selected| mux.backend_session_by_id_or_name(selected))
                    .and_then(|session| session.anchor.cwd.clone())
                    .unwrap_or_else(|| {
                        if remote.is_some() {
                            String::new()
                        } else {
                            self.config()
                                .session
                                .working_directory
                                .as_ref()
                                .map(|path| path.to_string_lossy().into_owned())
                                .or_else(|| {
                                    bootty_git::home_dir()
                                        .map(|path| path.to_string_lossy().into_owned())
                                })
                                .unwrap_or_default()
                        }
                    });
                let space = spaces.iter().find(|space| space.id == scope)?;
                Some(SessionDestination {
                    scope,
                    label: format!(
                        "{} · {}",
                        space.name,
                        remote.as_ref().map_or("Local", |remote| remote.host())
                    ),
                    icon: space.icon.clone(),
                    color: space.color,
                    cwd,
                    remote,
                    target,
                })
            })
            .collect()
    }
    pub(super) fn open_add_project_dialog(&mut self, effects: &mut Vec<AppEffect>) {
        self.close_overlay_dialogs();
        let target = self
            .current_command_target_for("project.register", bootty_control::ResourceKind::Binding);
        if self.active_multiplexer().remote.is_none() {
            if let Some(target) = target {
                effects.push(AppEffect::ChooseProjectDirectory(target));
            }
            return;
        }
        self.show_overlay(ModalDialog::NewSession(Box::new(
            self.active_multiplexer()
                .remote
                .clone()
                .map_or_else(
                    || NewSessionDialog::open_add_project_local(self.repaint.clone()),
                    |remote| {
                        NewSessionDialog::open_add_project_remote(remote, self.repaint.clone())
                    },
                )
                .with_registration_target(target),
        )));
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
            .with_profiles(profiles.into_iter()),
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
            .with_profiles(profiles.into_iter()),
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
        let scope = self.mux_scope();
        let Some(identity) = self.workspace.session_identity(scope, session_id) else {
            return false;
        };
        let Some(saved) = self.workspace.active.binding.sessions().get(&identity) else {
            return false;
        };
        let name = saved.label().to_owned();
        self.open_overlay(ModalDialog::RenameSession(RenameSessionDialog::open(
            identity, name,
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
    pub(super) fn open_keybind_help_dialog(&mut self) {
        let bindings = self.config().input.keybinds();
        self.open_overlay(ModalDialog::KeybindHelp(KeybindHelpDialog::open(&bindings)));
    }
    pub(super) fn open_command_palette_dialog(&mut self) {
        let bindings = self.config().input.keybinds();
        let descriptors = self.commands.catalog().list();
        self.open_overlay(ModalDialog::CommandPalette(
            CommandPaletteDialog::open_with_catalog(
                &bindings,
                CommandPaletteState {
                    appearance_mode: self.config().appearance.mode,
                    sidebar_visible: self.config().chrome.sidebar,
                },
                &self.localizer,
                &descriptors,
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
        if !matches!(self.dialogs.current(), Some(ModalDialog::NewSession(_))) {
            self.close_overlay_dialogs();
        }
        self.theme_picker_restore_config = Some((restore_config, self.active_appearance_variant));
        self.show_overlay(ModalDialog::ThemePicker(ThemePickerDialog::open(
            names,
            current,
            branch.to_owned(),
        )));
    }
}
