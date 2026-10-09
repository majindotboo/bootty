use std::{sync::mpsc, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_mux::{backend::PaneInput, target::ExactMuxTarget};

use super::{
    CommandDispatch, CommandExecutor, CoreCommandExecutor, PendingCommandResult,
    command_outcome_for_mux_error,
};
use crate::{AppState, app_actions::KeybindAction, commands::SynchronousCommand};

/// The pane input a terminal command performs when it names its own target.
pub(super) fn targeted_pane_input(executor: &CommandExecutor) -> Option<PaneInput> {
    match executor {
        CommandExecutor::Core(CoreCommandExecutor::Keybind(KeybindAction::Write(bytes))) => {
            Some(PaneInput::Write(bytes.clone()))
        }
        CommandExecutor::Core(CoreCommandExecutor::Synchronous(
            SynchronousCommand::PasteTerminal(text),
        )) => Some(PaneInput::Paste(text.clone())),
        CommandExecutor::Core(CoreCommandExecutor::Synchronous(
            SynchronousCommand::SubmitTerminal,
        )) => Some(PaneInput::Submit),
        _ => None,
    }
}

impl AppState {
    /// Deliver input to a named pane in any Space without selecting its window or moving focus.
    pub(super) fn dispatch_pane_input(
        &mut self,
        exact: &ExactMuxTarget,
        input: PaneInput,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (_, _, Some(pane)) = exact.ids() else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "Terminal input requires a pane target".to_owned(),
            });
        };
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        let scope = exact.scope();
        let records_activity = match &input {
            PaneInput::Write(bytes) => !bytes.is_empty(),
            PaneInput::Paste(text) => !text.is_empty(),
            PaneInput::Submit => true,
        };
        let receipt = records_activity
            .then(|| exact.ids().0)
            .flatten()
            .and_then(|session| self.workspace.session_identity(scope, session))
            .and_then(|identity| {
                self.saved_session_invocation(
                    scope,
                    "session.activity",
                    vec![identity, "0".to_owned()],
                )
            });
        let commands = self.commands.sender.clone();
        self.workspace
            .send_pane_input(scope, pane, input, execution, move |outcome| {
                if outcome.is_ok()
                    && let Some(receipt) = receipt
                {
                    super::sessions::submit_activity_receipt(
                        &commands,
                        receipt,
                        crate::clock::ClockSnapshot::now().epoch,
                    );
                }
                let _ = sender.send(outcome.map_or_else(command_outcome_for_mux_error, |()| {
                    CommandOutcome::success()
                }));
                repaint();
            });
        CommandDispatch::Pending(PendingCommandResult::Outcome(result))
    }
}
