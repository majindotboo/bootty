//! Pane-addressed input and capture that never move selection or UI focus.

use std::{thread, time::Instant};

use anyhow::{Result, anyhow};
use bootty_control::CommandCancellation;
use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};

use super::{BindingRuntime, WorkspaceRuntime};
use crate::{
    backend::{MuxBackend, PaneCapture, PaneInput, PaneText},
    controller::{MuxCommandError, SpaceId},
    executor::begin_synchronous_command,
    provider::PaneTopology,
    target::ExactMuxTarget,
    terminal::TerminalRuntime,
};

type Execution = Option<(Instant, CommandCancellation)>;

impl WorkspaceRuntime {
    /// Prepare the exact process-local pane for I/O, wherever its Space's owner lives.
    /// This starts the terminal in the background without changing selection or pane layout.
    /// # Errors
    /// Returns an error if the pane is absent, not held by this Space, or cannot start.
    pub fn prepare_space_terminal_runtime(&mut self, target: &ExactMuxTarget) -> Result<()> {
        let ExactMuxTarget::Pane(scope, session, window, pane) = target else {
            anyhow::bail!("terminal preparation requires an exact pane target");
        };
        let binding = self
            .binding(*scope)
            .ok_or_else(|| anyhow!("the target Space was closed"))?;
        anyhow::ensure!(
            binding.backend_policy.panes.topology == PaneTopology::ProcessLocal,
            "this backend owns its pane processes"
        );
        let anchor = binding
            .mux
            .sessions()
            .iter()
            .find(|candidate| candidate.id == *session)
            .and_then(|session| {
                session
                    .windows
                    .iter()
                    .find(|candidate| candidate.id == *window)
            })
            .and_then(|window| {
                window
                    .panes
                    .iter()
                    .find(|candidate| candidate.pane_id.as_deref() == Some(pane))
            })
            .cloned()
            .ok_or_else(|| anyhow!("the requested pane is no longer held by this Space"))?;
        self.space_terminal_owner(*scope)?
            .terminal
            .prepare_scoped_native_pane(*scope, anchor)
    }

    /// Deliver input to one pane of the Space `scope`, leaving selection and UI focus alone, then
    /// report the result to `done`.
    ///
    /// A pane with a local terminal runtime takes the input there at once, wherever that runtime
    /// lives: the active owner, or the one holding an inactive Space's panes. It encodes keys for
    /// the pane's keyboard mode exactly as typing does. A backend pane without one, such as an
    /// rmux session never shown or any tmux pane, is addressed through the backend on a worker
    /// thread, so the UI never waits on a backend process. Either way the command is claimed
    /// right before it acts: an expired, cancelled, or stale command never reaches the pane.
    pub fn send_pane_input(
        &mut self,
        scope: SpaceId,
        pane_id: &str,
        input: PaneInput,
        execution: Execution,
        done: impl FnOnce(Result<(), MuxCommandError>) + Send + 'static,
    ) {
        let Some(topology) = self
            .binding(scope)
            .map(|binding| binding.backend_policy.panes.topology)
        else {
            done(Err(MuxCommandError::Stale));
            return;
        };
        if topology != PaneTopology::Attach {
            if let Some(runtime) = self.space_terminal_runtime(scope, pane_id) {
                done(
                    begin_synchronous_command(execution)
                        .and_then(|()| write_local_pane(runtime, &input).map_err(failed)),
                );
                return;
            }
            // A process-local pane exists only as its runtime.
            if topology == PaneTopology::ProcessLocal {
                done(
                    begin_synchronous_command(execution).and_then(|()| {
                        Err(failed(anyhow!("pane {pane_id} has no running terminal")))
                    }),
                );
                return;
            }
        }
        let Some(binding) = self.binding(scope) else {
            done(Err(MuxCommandError::Stale));
            return;
        };
        let pane_id = pane_id.to_owned();
        binding.on_backend_thread(execution, done, move |backend| {
            backend.send_pane_input(&pane_id, &input)
        });
    }
}

impl BindingRuntime {
    /// Whether this binding's backend owns its pane processes, so it can reach any pane by id
    /// whether or not Bootty shows it. A process-local pane exists only in its terminal runtime.
    #[must_use]
    pub fn addresses_backend_panes(&self) -> bool {
        self.backend_policy.panes.topology != PaneTopology::ProcessLocal
    }

    /// Read one backend pane's text through the backend, whether or not it is on screen.
    pub fn capture_backend_pane(
        &self,
        pane_id: &str,
        capture: PaneCapture,
        execution: Execution,
        done: impl FnOnce(Result<PaneText, MuxCommandError>) + Send + 'static,
    ) {
        let pane_id = pane_id.to_owned();
        self.on_backend_thread(execution, done, move |backend| {
            backend.capture_pane(&pane_id, capture)
        });
    }

    /// Run `run` against a fresh backend on a worker thread. It is claimed after the backend is
    /// built, right before it acts, and fails as stale if this binding was closed or replaced in
    /// the meantime.
    fn on_backend_thread<T: Send + 'static>(
        &self,
        execution: Execution,
        done: impl FnOnce(Result<T, MuxCommandError>) + Send + 'static,
        run: impl FnOnce(&dyn MuxBackend) -> Result<T> + Send + 'static,
    ) {
        let backends = self.backends.clone();
        let config = self.multiplexer.clone();
        let workspace = self.mux.workspace_path().map(std::path::Path::to_path_buf);
        let fence = self.mux.command_fence(&config);
        thread::spawn(move || {
            let result = backends
                .build_backend(&config, workspace.as_deref())
                .map_err(failed)
                .and_then(|backend| {
                    fence.claim()?;
                    begin_synchronous_command(execution)?;
                    run(backend.as_ref()).map_err(failed)
                });
            done(result);
        });
    }
}

fn write_local_pane(runtime: &mut dyn TerminalRuntime, input: &PaneInput) -> Result<()> {
    match input {
        PaneInput::Write(bytes) => runtime.write_input(bytes),
        PaneInput::Paste(text) => runtime.write_paste(text),
        PaneInput::Submit => runtime.encode_key(KeyInput {
            key: TerminalKey::Enter,
            mods: KeyMods::default(),
            repeat: false,
            utf8: None,
            unshifted: None,
        }),
    }
}

#[allow(clippy::needless_pass_by_value, reason = "used as a `map_err` adapter")]
fn failed(error: anyhow::Error) -> MuxCommandError {
    MuxCommandError::Failed(format!("{error:#}"))
}
