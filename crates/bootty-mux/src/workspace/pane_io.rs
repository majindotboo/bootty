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
    terminal::TerminalRuntime,
};

type Execution = Option<(Instant, CommandCancellation)>;

impl WorkspaceRuntime {
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
        let scope = *scope;
        let pane_id = pane_id.as_str();
        let Some(binding) = self.binding(scope) else {
            done(Err(MuxCommandError::Stale));
            return;
        };
        if binding.backend_policy.panes.topology == PaneTopology::ProcessLocal {
            let pane = binding
                .mux
                .all_sessions()
                .iter()
                .flat_map(|session| &session.windows)
                .flat_map(|window| std::iter::once(&window.anchor).chain(&window.panes))
                .find(|pane| pane.pane_id.as_deref() == Some(pane_id))
                .cloned();
            let result = begin_synchronous_command(execution).and_then(|()| {
                let pane = pane.ok_or(MuxCommandError::Stale)?;
                self.space_terminal_owner(scope)
                    .map_err(failed)?
                    .terminal
                    .replace_scoped_restored_native(scope, pane, argv, cwd, history)
                    .map_err(failed)
            });
            done(result);
            return;
        }
        let pane_id = pane_id.to_owned();
        binding.on_backend_thread(execution, done, move |backend| {
            backend.respawn_pane_command(&pane_id, &argv, cwd.as_deref())
        });
    }

    /// Reattach a backend process after accepted respawn, queuing history after its reset rebase.
    /// # Errors
    /// Returns missing binding/topology or renderer attachment failures.
    pub fn restore_respawned_pane_history(
        &mut self,
        scope: SpaceId,
        pane_id: &str,
        history: &str,
    ) -> Result<()> {
        let binding = self
            .binding_mut(scope)
            .ok_or_else(|| anyhow!("respawned binding is unavailable"))?;
        if binding.backend_policy.panes.topology == PaneTopology::ProcessLocal {
            return Ok(());
        }
        let mapping = binding
            .restored_sessions
            .values_mut()
            .find(|mapping| mapping.panes.values().any(|pane| pane == pane_id))
            .ok_or_else(|| anyhow!("respawned pane has no restored topology"))?;
        mapping.history.remove(pane_id);
        binding.terminal_mut().queue_scoped_restored_history(
            scope,
            pane_id,
            std::sync::Arc::from(history),
        );
        binding.terminal_mut().discard_scoped_pane(scope, pane_id);
        binding.sync_terminal_panes()
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
        if let Some(binding) = self.binding_mut(scope) {
            binding.revoke_restored_agent_terminal(pane_id);
        }
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

    /// Capture a backend's latest complete styled rows within the checkpoint byte budget.
    pub fn capture_checkpoint_pane(
        &self,
        pane_id: &str,
        options: bootty_terminal::terminal_capture::CaptureOptions,
        done: impl FnOnce(Result<bootty_terminal::terminal_capture::TerminalCapture, MuxCommandError>)
        + Send
        + 'static,
    ) {
        let pending = self
            .terminal_owner
            .terminal
            .pending_scoped_restored_history(self.scope, pane_id)
            .map(|text| {
                let saved = self
                    .restored_sessions
                    .iter()
                    .find_map(|(identity, mapping)| {
                        let logical = mapping
                            .panes
                            .iter()
                            .find(|(_, backend)| backend.as_str() == pane_id)?
                            .0;
                        self.sessions
                            .get(identity)?
                            .terminal_snapshot
                            .as_ref()?
                            .windows
                            .iter()
                            .flat_map(|window| &window.panes)
                            .find(|pane| &pane.id == logical)
                    });
                (
                    text,
                    saved.map_or(80, |pane| if pane.cols == 0 { 80 } else { pane.cols }),
                    saved.map_or(0, |pane| pane.omitted_lines),
                    self.terminal_owner.terminal.terminal_colors(),
                )
            });
        let pane_id = pane_id.to_owned();
        self.on_backend_thread(None, done, move |backend| {
            let captured =
                bootty_terminal::terminal_history::capture_checkpoint(options, |options| {
                    let captured = backend.capture_pane(
                        &pane_id,
                        PaneCapture {
                            history: true,
                            ansi: true,
                            max_lines: options.max_lines,
                        },
                    )?;
                    Ok(bootty_terminal::terminal_capture::TerminalCapture {
                        cols: 0,
                        rows: 0,
                        scope: options.scope,
                        format: options.format,
                        alternate_screen: false,
                        captured_lines: u32::try_from(captured.captured_lines).unwrap_or(u32::MAX),
                        omitted_lines: captured.omitted_lines,
                        text: captured.text,
                    })
                })?;
            let Some((history, cols, omitted, colors)) = pending else {
                return Ok(captured);
            };
            // A hidden cold pane has no reader yet. Format its saved prefix and fresh backend
            // rows together; never checkpoint only the new shell over its retained history.
            // Scratch storage is bounded; increase it only if the saved-history budget grows.
            let mut engine = bootty_terminal::terminal_engine::TerminalEngine::new_with_scrollback(
                bootty_terminal::geometry::TerminalGeometry {
                    cols,
                    rows: 1,
                    cell_width: 8,
                    cell_height: 16,
                },
                colors,
                32 * 1024 * 1024,
            )?;
            engine.write_vt_without_pty_responses(
                &bootty_terminal::terminal_history::styled_history_bytes(&history)?,
            );
            engine.write_vt_without_pty_responses(
                &bootty_terminal::terminal_history::styled_history_bytes(&captured.text)?,
            );
            let mut merged = engine.capture_checkpoint(options)?;
            // The backend API supplies no geometry; do not advertise the formatting viewport.
            merged.cols = 0;
            merged.rows = 0;
            merged.omitted_lines = merged
                .omitted_lines
                .saturating_add(omitted)
                .saturating_add(captured.omitted_lines);
            Ok(merged)
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
