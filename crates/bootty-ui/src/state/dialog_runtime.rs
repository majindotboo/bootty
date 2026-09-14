use crate::presentation::dialogs::{
    CommandPaletteDialog, DitchSessionDialog, KeybindHelpDialog, NewSessionDialog,
    RenameSessionDialog, RenameTabDialog, SessionPickerDialog, SpaceEditorDialog,
    SpacePickerDialog, ThemePickerDialog,
};

/// The one product workflow presented as a floating modal in an application window.
pub enum ModalDialog {
    NewSession(NewSessionDialog),
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

#[derive(Default)]
pub(super) struct DialogRuntime {
    modal: Option<Box<ModalDialog>>,
}

impl DialogRuntime {
    pub(super) fn open(&mut self, dialog: ModalDialog) {
        self.modal = Some(Box::new(dialog));
    }

    pub(super) fn clear(&mut self) {
        self.modal = None;
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
                    | ModalDialog::SpaceEditor(_)
                    | ModalDialog::SessionPicker(_)
                    | ModalDialog::RenameSession(_)
                    | ModalDialog::RenameTab(_)
                    | ModalDialog::DitchSession(_)
            )
        ) {
            self.modal = None;
        }
    }
}
