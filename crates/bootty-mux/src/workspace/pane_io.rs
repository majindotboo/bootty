//! Pane-addressed input and capture that never move selection or UI focus.

use std::time::Instant;

use anyhow::Result;
use bootty_control::CommandCancellation;

use super::WorkspaceRuntime;
use crate::{
    command::PaneInput,
    controller::{MuxCommandError, SpaceId},
    executor::begin_synchronous_command,
};

type Execution = Option<(Instant, CommandCancellation)>;

impl WorkspaceRuntime {
    /// Capture from the pane owner without requiring an admitted rendering VT.
    /// # Errors
    /// Rejects a closed binding or a stopped checkpoint worker.
    pub fn capture_session_pane(
        &self,
        scope: SpaceId,
        pane: &str,
        options: bootty_terminal::terminal_capture::CaptureOptions,
    ) -> Result<
        bootty_terminal::terminal_session::PendingWorkerResponse<
            Result<crate::session_snapshot::SessionPaneCapture, String>,
        >,
    > {
        let binding = self
            .binding(scope)
            .ok_or_else(|| anyhow::anyhow!("checkpoint binding was closed"))?;
        let backend = binding.mux.native_backend();
        let fence = binding.mux.command_fence(binding.multiplexer());
        let pane = pane.to_owned();
        let repaint = std::sync::Arc::clone(&self.repaint);
        let (done, response) = bootty_terminal::terminal_session::worker_request();
        std::thread::spawn(move || {
            if !done.try_claim() {
                return;
            }
            let result = (|| {
                fence.claim()?;
                match backend.request(&crate::native::protocol::Request::CaptureCheckpoint {
                    pane,
                    options,
                })? {
                    crate::native::protocol::Response::CheckpointCaptured(capture) => Ok(capture),
                    _ => anyhow::bail!("native mux returned no checkpoint capture"),
                }
            })()
            .map_err(|error: anyhow::Error| format!("{error:#}"));
            done.send(result);
            repaint();
        });
        Ok(response)
    }

    /// Replace a pane process through its owner; this never writes launch text to a terminal.
    pub fn respawn_pane_command(
        &mut self,
        target: &crate::target::ExactMuxTarget,
        argv: Vec<String>,
        cwd: Option<String>,
        history: std::sync::Arc<str>,
        execution: Execution,
        done: impl FnOnce(Result<(), MuxCommandError>) + Send + 'static,
    ) {
        let crate::target::ExactMuxTarget::Pane(scope, _, _, pane_id) = target else {
            done(Err(MuxCommandError::Stale));
            return;
        };
        let Some(binding) = self.binding(*scope) else {
            done(Err(MuxCommandError::Stale));
            return;
        };
        let backend = binding.mux.native_backend();
        let pane = pane_id.clone();
        let mut launch = backend.launch_config();
        launch.command = argv;
        launch.command_is_argv = true;
        launch.working_directory = cwd.map(Into::into);
        let begin = begin_synchronous_command(execution);
        std::thread::spawn(move || {
            let result = begin.and_then(|()| {
                backend
                    .request(&crate::native::protocol::Request::Respawn {
                        pane,
                        launch,
                        history: history.to_string(),
                    })
                    .map(|_| ())
                    .map_err(|e| failed(&e))
            });
            done(result);
        });
    }

    pub fn send_pane_input(
        &mut self,
        scope: SpaceId,
        pane_id: &str,
        input: &PaneInput,
        execution: Execution,
        done: impl FnOnce(Result<(), MuxCommandError>) + Send + 'static,
    ) {
        let Some(binding) = self.binding(scope) else {
            done(Err(MuxCommandError::Stale));
            return;
        };
        let backend = binding.mux.native_backend();
        let pane = pane_id.to_owned();
        let input = input.clone();
        let begin = begin_synchronous_command(execution);
        std::thread::spawn(move || {
            let result = begin.and_then(|()| {
                backend
                    .request(&crate::native::protocol::Request::Input { pane, input })
                    .map(|_| ())
                    .map_err(|e| failed(&e))
            });
            done(result);
        });
    }
}

fn failed(error: &anyhow::Error) -> MuxCommandError {
    MuxCommandError::Failed(format!("{error:#}"))
}
