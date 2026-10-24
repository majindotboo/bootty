use crate::presentation::dialogs::{
    CommandPaletteDialog, KeybindHelpDialog, NewSessionDialog, RenameSessionDialog,
    RenameTabDialog, SessionPickerDialog, SpaceEditorDialog, SpacePickerDialog, ThemePickerDialog,
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
    pub(super) creation_drafts:
        std::collections::BTreeMap<String, super::creation_draft::CreationDraft>,
    pub(super) restore_creation: Option<String>,
    pub(super) automatic_creation: bool,
    pub(super) creation_revision: u64,
    pub(super) new_session_isolated: bool,
    pub(super) command_palette_native_parent: Option<bootty_control::CommandTarget>,
}

impl DialogRuntime {
    pub(super) fn with_creation_drafts(
        drafts: Vec<super::creation_draft::CreationDraft>,
        selected: Option<String>,
    ) -> Self {
        let drafts = drafts
            .into_iter()
            .map(|record| (record.draft.identity.clone(), record))
            .collect::<std::collections::BTreeMap<_, _>>();
        Self {
            new_session_isolated: selected
                .as_ref()
                .and_then(|id| drafts.get(id))
                .is_some_and(|record| record.draft.isolation_preference),
            creation_drafts: drafts,
            restore_creation: selected,
            ..Self::default()
        }
    }

    pub(super) fn remember_creation(&mut self) {
        let record = match self.modal.as_deref() {
            Some(ModalDialog::NewSession(dialog)) => dialog.retained_draft(),
            _ => None,
        };
        if let Some(record) = record {
            self.new_session_isolated = record.draft.isolation_preference;
            if record.has_content() {
                if self.creation_drafts.get(&record.draft.identity) != Some(&record) {
                    self.creation_drafts
                        .insert(record.draft.identity.clone(), record);
                    self.creation_revision = self.creation_revision.wrapping_add(1);
                }
            } else if self
                .creation_drafts
                .remove(&record.draft.identity)
                .is_some()
            {
                self.creation_revision = self.creation_revision.wrapping_add(1);
            }
        }
    }

    pub(super) fn creation_snapshot(
        &self,
    ) -> (
        std::collections::BTreeMap<String, super::creation_draft::CreationDraft>,
        Option<String>,
    ) {
        let mut records = self.creation_drafts.clone();
        let mut selected = None;
        for dialog in [self.creation.as_deref(), self.modal.as_deref()]
            .into_iter()
            .flatten()
        {
            if let ModalDialog::NewSession(dialog) = dialog
                && let Some(record) = dialog.retained_draft()
            {
                if record.has_content() {
                    selected = Some(record.draft.identity.clone());
                    records.insert(record.draft.identity.clone(), record);
                } else {
                    records.remove(&record.draft.identity);
                }
            }
        }
        (records, selected)
    }

    pub(super) fn active_creation_identity(&self) -> Option<&str> {
        [self.modal.as_deref(), self.creation.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|dialog| {
                if let ModalDialog::NewSession(dialog) = dialog {
                    dialog
                        .draft()
                        .filter(|draft| {
                            dialog.retains_creation_draft()
                                && (!self.automatic_creation
                                    || !draft.prompt.trim().is_empty()
                                    || !draft.command.trim().is_empty()
                                    || !draft.attachments.is_empty())
                        })
                        .map(|draft| draft.identity.as_str())
                } else {
                    None
                }
            })
    }

    pub(super) fn remove_creation_draft(&mut self, identity: &str) {
        let visible = self.modal.as_deref().is_some_and(|dialog| match dialog {
            ModalDialog::NewSession(dialog) => dialog
                .draft()
                .is_some_and(|draft| draft.identity == identity),
            _ => false,
        });
        if visible {
            self.clear();
        }
        if self.creation_drafts.remove(identity).is_some() {
            self.creation_revision = self.creation_revision.wrapping_add(1);
        }
        if self.restore_creation.as_deref() == Some(identity) {
            self.restore_creation = None;
        }
    }

    pub(super) fn open(&mut self, dialog: ModalDialog) {
        self.remember_creation();
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
        self.remember_creation();
        self.automatic_creation = false;
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
            )
        ) {
            self.clear();
        }
    }
}
