use crate::presentation::dialogs::{
    CommandPaletteDialog, DitchSessionDialog, KeybindHelpDialog, NewSessionDialog,
    RenameSessionDialog, RenameTabDialog, SessionPickerDialog, SpaceEditorDialog,
    SpacePickerDialog, ThemePickerDialog,
};

/// The one product workflow presented as a floating modal in an application window.
pub enum ModalDialog {
    NewSession(Box<NewSessionDialog>),
    TerminalHistory(Box<crate::presentation::terminal_history::TerminalHistoryDialog>),
    ThemeEditor(crate::presentation::theme_editor::ThemeEditorDialog),
    Capture(crate::presentation::capture::CaptureDialog),
    SpaceEditor(SpaceEditorDialog),
    SessionPicker(SessionPickerDialog),
    RenameSession(RenameSessionDialog),
    RenameTab(RenameTabDialog),
    DitchSession(DitchSessionDialog),
    KeybindHelp(KeybindHelpDialog),
    CommandPalette(CommandPaletteDialog),
    ThemePicker(ThemePickerDialog),
    SpacePicker(SpacePickerDialog),
}

pub(super) struct PendingSessionName {
    pub scope: bootty_mux::controller::SpaceId,
    pub identity: String,
    pub initial_title: String,
    pub native: Option<(bootty_control::CommandTarget, String)>,
    pub reply: std::sync::mpsc::Receiver<bootty_control::CommandOutcome>,
}

#[derive(Default)]
pub(super) struct DialogRuntime {
    modal: Option<Box<ModalDialog>>,
    creation: Option<Box<ModalDialog>>,
    pub empty_creation_scope: Option<bootty_mux::controller::SpaceId>,
    pub project_settings: Option<crate::presentation::project_editor::ProjectSettingsEditor>,
    pub session_names: Vec<PendingSessionName>,
    pub creation_replies: Vec<std::sync::mpsc::Receiver<bootty_control::CommandOutcome>>,
    pub(super) new_session_draft: Option<crate::presentation::new_session_form::NewSessionDraft>,
    pub(super) new_session_isolated: bool,
    pub(super) command_palette_native_parent: Option<bootty_control::CommandTarget>,
}

impl DialogRuntime {
    pub(super) fn open(&mut self, dialog: ModalDialog) {
        let overlays_creation = !matches!(dialog, ModalDialog::NewSession(_))
            && matches!(self.modal.as_deref(), Some(ModalDialog::NewSession(_)));
        if !self.is_dismissible() && !overlays_creation {
            return;
        }
        if overlays_creation {
            self.creation = self.modal.take();
        }
        if !matches!(dialog, ModalDialog::CommandPalette(_)) {
            self.command_palette_native_parent = None;
        }
        self.modal = Some(Box::new(dialog));
    }

    pub(super) fn clear(&mut self) {
        if !self.is_dismissible() {
            return;
        }
        if let Some(ModalDialog::NewSession(dialog)) = self.modal.as_deref()
            && dialog.retains_creation_draft()
            && let Some(draft) = dialog.draft()
        {
            self.new_session_isolated = draft.isolation_preference;
            self.new_session_draft = Some(draft.clone());
        }
        self.modal = self.creation.take();
        self.command_palette_native_parent = None;
    }

    pub(super) fn creation_spec(&self) -> Option<crate::gpui::DialogSpec> {
        match self.creation.as_deref() {
            Some(ModalDialog::NewSession(dialog)) => Some(dialog.spec()),
            _ => None,
        }
    }

    pub(super) fn is_dismissible(&self) -> bool {
        match self.modal.as_deref() {
            Some(ModalDialog::NewSession(dialog)) => !dialog.is_in_flight(),
            Some(ModalDialog::TerminalHistory(dialog)) => !dialog.is_in_flight(),
            _ => true,
        }
    }

    pub(super) const fn take(&mut self) -> Option<Box<ModalDialog>> {
        self.modal.take()
    }

    pub(super) fn replace(&mut self, dialog: Box<ModalDialog>) {
        self.modal = Some(dialog);
    }

    pub(super) const fn has_modal(&self) -> bool {
        self.modal.is_some()
    }

    pub(super) fn current(&self) -> Option<&ModalDialog> {
        self.modal.as_deref()
    }

    pub(super) fn current_mut(&mut self) -> Option<&mut ModalDialog> {
        self.modal.as_deref_mut()
    }

    pub(super) fn is_session_picker(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::SessionPicker(_)))
    }

    pub(super) fn is_command_palette(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::CommandPalette(_)))
    }

    pub(super) fn is_theme_picker(&self) -> bool {
        matches!(self.modal.as_deref(), Some(ModalDialog::ThemePicker(_)))
    }

    pub(super) fn command_palette(&self) -> Option<&CommandPaletteDialog> {
        match self.modal.as_deref() {
            Some(ModalDialog::CommandPalette(dialog)) => Some(dialog),
            _ => None,
        }
    }

    pub(super) fn clear_space_context(&mut self) {
        if matches!(
            self.modal.as_deref(),
            Some(
                ModalDialog::NewSession(_)
                    | ModalDialog::TerminalHistory(_)
                    | ModalDialog::SpaceEditor(_)
                    | ModalDialog::SessionPicker(_)
                    | ModalDialog::RenameSession(_)
                    | ModalDialog::RenameTab(_)
                    | ModalDialog::DitchSession(_)
            )
        ) {
            self.clear();
        }
    }
}
