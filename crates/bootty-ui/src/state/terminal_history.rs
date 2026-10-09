//! Provider history commands keep their issued destination through the modal workflow.

use bootty_agents::AgentKind;
use bootty_control::{Caller, CommandCancellation, CommandOutcome, ResourceKind};

use super::{AppState, dialog_runtime::ModalDialog};
use crate::commands::ExactMuxTarget;
use crate::presentation::terminal_history::{
    TerminalHistoryContext, TerminalHistoryDialog, TerminalHistoryEvent,
};

impl AppState {
    pub(crate) fn open_terminal_history(
        &mut self,
        provider: AgentKind,
        exact: Option<&ExactMuxTarget>,
    ) -> CommandOutcome {
        if !self.dialogs.is_dismissible() {
            return CommandOutcome::Unavailable {
                message: "Wait for the current session to finish starting".to_owned(),
            };
        }
        let Some(ExactMuxTarget::Session(scope, id)) = exact else {
            return CommandOutcome::Unavailable {
                message: "Select a session before opening provider history".to_owned(),
            };
        };
        let Some(binding) = self.workspace.binding(*scope) else {
            return CommandOutcome::StaleTarget {
                message: "The history destination is no longer available".to_owned(),
            };
        };
        let mux = binding.mux();
        let Some(session) = self.mux_resource_target(*scope, ResourceKind::Session, id, None)
        else {
            return CommandOutcome::StaleTarget {
                message: "The selected session is no longer available".to_owned(),
            };
        };
        let handle = self.binding_target_handle(*scope, mux.binding_generation());
        let Some(target) =
            ExactMuxTarget::Binding(*scope).command_target(ResourceKind::Binding, mux, &handle)
        else {
            return CommandOutcome::StaleTarget {
                message: "The history host is no longer available".to_owned(),
            };
        };
        let Some(preferences) = self.config().agents.provider(&provider.to_string()) else {
            return CommandOutcome::Unsupported {
                message: "Provider history is unavailable".to_owned(),
            };
        };
        let profile = preferences.selected_profile();
        let context = TerminalHistoryContext {
            provider,
            binding: target,
            session,
            cwd: mux
                .backend_session_by_id_or_name(id)
                .and_then(|session| session.anchor.cwd.clone()),
            profile: preferences.selected.clone(),
            program: if preferences.program.is_empty() {
                provider.default_program().to_owned()
            } else {
                preferences.program.clone()
            },
            arguments: profile.map_or_else(Vec::new, |profile| profile.arguments.clone()),
        };
        let mut dialog = TerminalHistoryDialog::new(context);
        let request = dialog.query();
        self.close_overlay_dialogs();
        self.dialogs
            .open(ModalDialog::TerminalHistory(Box::new(dialog)));
        if let Some(request) = request {
            self.apply_terminal_history_event(request);
        }
        CommandOutcome::Success {
            value: serde_json::json!({"opened": true}),
            warnings: Vec::new(),
        }
    }

    pub(super) fn apply_terminal_history_event(&mut self, event: TerminalHistoryEvent) {
        match event {
            TerminalHistoryEvent::Close => {
                self.dismiss_modal_dialog();
            }
            TerminalHistoryEvent::Request {
                invocation,
                opening,
            } => {
                let now = std::time::Instant::now();
                let result = self.app_command_sender(Caller::Internal).submit(
                    invocation,
                    now.checked_add(std::time::Duration::from_secs(30))
                        .unwrap_or(now),
                    CommandCancellation::new(),
                );
                if let Some(ModalDialog::TerminalHistory(dialog)) = self.dialogs.current_mut() {
                    match result {
                        Ok(response) => dialog.started(response, opening),
                        Err(error) => {
                            dialog.failed(format!("Provider command could not start: {error:?}"));
                        }
                    }
                }
            }
        }
    }
}
