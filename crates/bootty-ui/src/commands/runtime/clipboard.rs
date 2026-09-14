use std::{sync::mpsc, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome, CommandTarget, ResourceKind};
use bootty_host::{CancellableCommandRunner, remote::RemoteHost};
use bootty_mux::{controller::SpaceId, executor, target, terminal::TerminalRuntime};

use crate::{
    AppState,
    platform::{ClipboardContent, prepare_clipboard_paste, read_clipboard_content},
};

use super::{CommandDispatch, PendingCommandResult};

pub(super) fn upload_failure(message: String) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "clipboard_paste_failed".to_owned(),
        message,
    }
}

impl AppState {
    pub(super) fn dispatch_clipboard_paste(
        &mut self,
        scope: SpaceId,
        target: CommandTarget,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        if cancellation.is_cancelled() {
            return CommandDispatch::Complete(CommandOutcome::cancelled());
        }
        let content = match read_clipboard_content() {
            Ok(Some(content)) => content,
            Ok(None) => return CommandDispatch::Complete(CommandOutcome::success()),
            Err(error) => return CommandDispatch::Complete(upload_failure(error.to_string())),
        };
        if let ClipboardContent::Text(text) = content {
            if let Err(error) = executor::begin_synchronous_command(Some((deadline, cancellation)))
            {
                return CommandDispatch::Complete(super::command_outcome_for_mux_error(error));
            }
            return CommandDispatch::Complete(self.finish_clipboard_paste(scope, &target, &text));
        }
        let remote = self
            .workspace
            .binding(scope)
            .and_then(|binding| binding.multiplexer().remote.clone())
            .map(RemoteHost::new);
        let runner = CancellableCommandRunner::with_deadline_and_cancellation_check(
            bootty_host::CommandCancellation::default(),
            deadline,
            move || cancellation.is_cancelled(),
        );
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let outcome = prepare_clipboard_paste(content, remote.as_ref(), &runner)
                .map_err(|error| error.to_string());
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Clipboard {
            scope,
            target,
            result,
        })
    }

    /// Resolve the original generation again. Never switch focus or substitute the active pane.
    pub(super) fn finish_clipboard_paste(
        &mut self,
        scope: SpaceId,
        target: &CommandTarget,
        text: &str,
    ) -> CommandOutcome {
        let Some(binding) = self.workspace.binding(scope) else {
            return CommandOutcome::StaleTarget {
                message: "clipboard destination was closed".to_owned(),
            };
        };
        let handle = self.binding_target_handle(scope, binding.mux().binding_generation());
        let exact = target::exact_mux_target(scope, binding.mux(), target, &handle);
        let current = self.current_command_target(ResourceKind::Terminal).as_ref() == Some(target)
            && scope == self.workspace.active.binding.scope();
        let Some(binding) = self.workspace.binding_mut(scope) else {
            return CommandOutcome::StaleTarget {
                message: "clipboard destination was closed".to_owned(),
            };
        };
        let terminal = binding.terminal_mut();
        let runtime: &mut dyn TerminalRuntime =
            if let Some(target::ExactMuxTarget::Pane(_, _, _, pane)) = exact {
                let Some(runtime) = terminal.focused_terminal_runtime(&pane) else {
                    return CommandOutcome::Unavailable {
                        message:
                            "clipboard destination is no longer attached; image was not pasted"
                                .to_owned(),
                    };
                };
                runtime
            } else if current {
                terminal
            } else {
                return CommandOutcome::StaleTarget {
                    message: "clipboard destination changed; image was not pasted".to_owned(),
                };
            };
        match runtime.write_paste(text) {
            Ok(()) => {
                (self.repaint)();
                CommandOutcome::success()
            }
            Err(error) => upload_failure(error.to_string()),
        }
    }
}
