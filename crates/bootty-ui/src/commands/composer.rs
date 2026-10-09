//! Commands completed by the window owning the active completion menu.
use crate::gpui::CommandAction;
use bootty_control::{CommandCancellation, CommandOutcome};
use std::{
    sync::{Arc, mpsc::Sender},
    time::Instant,
};
#[derive(Clone, Debug)]
pub struct ComposerRequest {
    pub action: CommandAction,
    execution: Option<(Instant, CommandCancellation)>,
    response: Arc<Sender<CommandOutcome>>,
}
impl PartialEq for ComposerRequest {
    fn eq(&self, other: &Self) -> bool {
        self.action == other.action && Arc::ptr_eq(&self.response, &other.response)
    }
}
impl ComposerRequest {
    pub(crate) fn new(
        action: CommandAction,
        execution: Option<(Instant, CommandCancellation)>,
        response: Sender<CommandOutcome>,
    ) -> Self {
        Self {
            action,
            execution,
            response: Arc::new(response),
        }
    }
    pub(crate) fn begin(&self) -> Result<(), bootty_mux::controller::MuxCommandError> {
        bootty_mux::executor::begin_synchronous_command(self.execution.clone())
    }
    pub(crate) fn complete(self, outcome: CommandOutcome) {
        _ = self.response.send(outcome);
    }
}
