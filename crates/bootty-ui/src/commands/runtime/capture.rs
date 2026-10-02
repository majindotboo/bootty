use std::{
    io::Write as _,
    path::Path,
    sync::mpsc,
    task::{Poll, ready},
    time::Instant,
};

use bootty_control::{CommandCancellation, CommandOutcome, CommandTarget, ResourceKind};
use bootty_mux::{
    backend::PaneCapture, executor, target::ExactMuxTarget, terminal::TerminalRuntime,
};
use bootty_terminal::{
    terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope, TerminalCapture},
    terminal_session::PendingWorkerResponse,
};

use super::{CommandDispatch, PendingAppCommand, PendingCommandResult};
use crate::AppState;
use crate::state::AppEffect;

fn failure(message: impl Into<String>) -> CommandOutcome {
    CommandOutcome::Failed {
        code: "terminal_capture_failed".to_owned(),
        message: message.into(),
    }
}

impl AppState {
    pub(super) fn dispatch_terminal_capture(
        &mut self,
        exact: &ExactMuxTarget,
        target: CommandTarget,
        arguments: &[String],
        export: bool,
        execution: Option<(Instant, CommandCancellation)>,
    ) -> CommandDispatch {
        let (deadline, cancellation) = executor::command_execution(execution);
        if cancellation.is_cancelled() {
            return CommandDispatch::Complete(CommandOutcome::cancelled());
        }
        if Instant::now() >= deadline {
            return CommandDispatch::Complete(CommandOutcome::deadline_exceeded());
        }
        let (destination, args) = if export {
            let Some((destination, args)) = arguments.split_first() else {
                return CommandDispatch::Complete(failure("Export destination is required"));
            };
            (Some(destination.clone()), args)
        } else {
            (None, arguments)
        };
        let options = capture_options(args, export);
        if let Err(message) = options.validate() {
            return CommandDispatch::Complete(failure(message));
        }
        let current = self
            .current_exact_mux_target_for("terminal.capture", ResourceKind::Terminal)
            .as_ref()
            == Some(exact);
        let Some(binding) = self.workspace.binding(exact.scope()) else {
            return CommandDispatch::Complete(failure("Capture binding was closed"));
        };
        let host = binding
            .multiplexer()
            .remote
            .as_ref()
            .map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label);
        let backend = format!("{:?}", binding.multiplexer().backend).to_lowercase();
        let native = binding.uses_native_terminal_layout();
        let backend_panes = binding.addresses_backend_panes();
        // A pane runtime is read wherever it lives, on screen or not, in any Space; an attached
        // client only when it shows the target. A backend pane with neither, such as an rmux
        // session never shown or a tmux pane off screen, is read by its backend.
        let has_runtime = native
            && exact.ids().2.is_some_and(|pane| {
                self.workspace
                    .space_terminal_runtime(exact.scope(), pane)
                    .is_some()
            });
        if backend_panes && !has_runtime && (native || !current) {
            return self.dispatch_backend_capture(
                exact,
                target,
                options,
                destination,
                (deadline, cancellation),
            );
        }
        if native
            && !backend_panes
            && !has_runtime
            && let Err(error) = self.workspace.prepare_space_terminal_runtime(exact)
        {
            return CommandDispatch::Complete(failure(error.to_string()));
        }
        let terminal: Option<&mut dyn TerminalRuntime> = if native {
            exact
                .ids()
                .2
                .and_then(|pane| self.workspace.space_terminal_runtime(exact.scope(), pane))
        } else {
            self.workspace
                .binding_mut(exact.scope())
                .map(|binding| -> &mut dyn TerminalRuntime { binding.terminal_mut() })
        };
        let Some(terminal) = terminal else {
            return CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "The requested terminal has no attached capture runtime".to_owned(),
            });
        };
        match terminal.started() {
            Ok(true) => {}
            Ok(false) => {
                return CommandDispatch::Pending(PendingCommandResult::CaptureStart {
                    exact: exact.clone(),
                    target,
                    arguments: arguments.to_vec(),
                    export,
                });
            }
            Err(error) => return CommandDispatch::Complete(failure(error.to_string())),
        }
        let pending = match terminal.capture(options) {
            Ok(pending) => pending,
            Err(error) => return CommandDispatch::Complete(failure(error.to_string())),
        };
        let source = serde_json::json!({
            "host": host,
            "backend": backend,
            "kind": if native { "pane_render_state" } else { "client_attachment_render_state" },
            "original_output": false,
        });
        self.dispatch_runtime_capture(
            pending,
            target,
            source,
            destination,
            (deadline, cancellation),
        )
    }

    fn dispatch_runtime_capture(
        &self,
        pending: PendingWorkerResponse<Result<TerminalCapture, String>>,
        target: CommandTarget,
        source: serde_json::Value,
        destination: Option<String>,
        (deadline, cancellation): (Instant, CommandCancellation),
    ) -> CommandDispatch {
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = pending
                .receive("capturing terminal")
                .and_then(|result| result.map_err(anyhow::Error::msg))
                .and_then(|mut capture| {
                    // Capture itself is read-only. Claim the mutation only when ready to publish,
                    // allowing a dismissed form or timed-out caller to cancel while formatting.
                    executor::begin_synchronous_command(Some((deadline, cancellation))).map_err(
                        |error| anyhow::anyhow!("Capture stopped before export: {error:?}"),
                    )?;
                    let bytes = capture.text.len();
                    if let Some(path) = &destination {
                        write_capture(Path::new(path), capture.text.as_bytes())?;
                        capture.text.clear();
                    }
                    Ok(serde_json::json!({
                        "capture": capture,
                        "bytes": bytes,
                        "target": target,
                        "source": source,
                        "destination": destination,
                    }))
                });
            let outcome = match result {
                Ok(value) => CommandOutcome::Success {
                    value,
                    warnings: Vec::new(),
                },
                Err(error) => failure(error.to_string()),
            };
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(result))
    }

    pub(super) fn poll_pending_terminal_capture(
        &mut self,
        pending: &mut PendingAppCommand,
        now: Instant,
        effects: &mut Vec<AppEffect>,
    ) -> Poll<Option<CommandOutcome>> {
        let PendingCommandResult::CaptureStart {
            exact,
            target,
            arguments,
            export,
        } = &pending.result
        else {
            return Poll::Ready(Some(failure("Capture startup request was lost")));
        };
        match ready!(self.poll_terminal_capture_start(
            exact,
            target,
            arguments,
            *export,
            (pending.deadline, pending.cancellation.clone()),
        )) {
            CommandDispatch::Complete(outcome) => Poll::Ready(Some(outcome)),
            CommandDispatch::Pending(result) => {
                pending.result = result;
                self.poll_pending_app_command(pending, now, effects)
            }
        }
    }

    pub(super) fn poll_terminal_capture_start(
        &mut self,
        exact: &ExactMuxTarget,
        target: &CommandTarget,
        arguments: &[String],
        export: bool,
        execution: (Instant, CommandCancellation),
    ) -> Poll<CommandDispatch> {
        match self.resolve_command_target(
            "terminal.capture",
            Some(ResourceKind::Terminal),
            Some(target),
        ) {
            Ok((_, Some(resolved))) if &resolved == exact => {}
            Ok(_) => {
                return Poll::Ready(CommandDispatch::Complete(CommandOutcome::StaleTarget {
                    message: "The requested terminal changed while starting".to_owned(),
                }));
            }
            Err(outcome) => return Poll::Ready(CommandDispatch::Complete(outcome)),
        }
        let terminal = exact
            .ids()
            .2
            .and_then(|pane| self.workspace.space_terminal_runtime(exact.scope(), pane));
        let Some(terminal) = terminal else {
            return Poll::Ready(CommandDispatch::Complete(CommandOutcome::Unavailable {
                message: "The requested terminal was closed while starting".to_owned(),
            }));
        };
        match terminal.started() {
            Ok(false) => Poll::Pending,
            Err(error) => Poll::Ready(CommandDispatch::Complete(failure(error.to_string()))),
            Ok(true) => Poll::Ready(self.dispatch_terminal_capture(
                exact,
                target.clone(),
                arguments,
                export,
                Some(execution),
            )),
        }
    }

    /// Capture an attached backend pane that is not on screen. It has no local render state, so
    /// the backend reads it directly and leaves its selection alone.
    fn dispatch_backend_capture(
        &self,
        exact: &ExactMuxTarget,
        target: CommandTarget,
        options: CaptureOptions,
        destination: Option<String>,
        execution: (Instant, CommandCancellation),
    ) -> CommandDispatch {
        let Some(pane) = exact.ids().2 else {
            return CommandDispatch::Complete(failure("Capture requires a pane target"));
        };
        if options.format == CaptureFormat::Html {
            return CommandDispatch::Complete(CommandOutcome::Unsupported {
                message: "HTML capture needs the pane on screen".to_owned(),
            });
        }
        let Some(binding) = self.workspace.binding(exact.scope()) else {
            return CommandDispatch::Complete(failure("Capture binding was closed"));
        };
        let source = serde_json::json!({
            "host": binding.multiplexer().remote.as_ref().map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label),
            "backend": format!("{:?}", binding.multiplexer().backend).to_lowercase(),
            "kind": "backend_pane",
            "original_output": false,
        });
        if let Err(message) = options.validate() {
            return CommandDispatch::Complete(failure(message));
        }
        let capture = PaneCapture {
            history: options.scope == CaptureScope::History,
            max_lines: options.max_lines,
            ansi: options.format == CaptureFormat::Ansi,
        };
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        // Reading stays cancellable; the command is claimed only right before it publishes, so a
        // dismissed or expired export is never written.
        binding.capture_backend_pane(pane, capture, None, move |captured| {
            let outcome = captured.map_or_else(super::command_outcome_for_mux_error, |pane| {
                if let Err(error) = executor::begin_synchronous_command(Some(execution)) {
                    return super::command_outcome_for_mux_error(error);
                }
                let bytes = pane.text.len();
                // Same bound as a terminal capture: fail rather than truncate.
                if bytes > options.max_bytes {
                    return failure(format!(
                        "Capture needs {bytes} bytes, exceeding the {} byte limit; request fewer lines",
                        options.max_bytes
                    ));
                }
                if let Some(path) = &destination
                    && let Err(error) = write_capture(Path::new(path), pane.text.as_bytes())
                {
                    return failure(error.to_string());
                }
                CommandOutcome::Success {
                    value: serde_json::json!({
                        "capture": {
                            "scope": options.scope,
                            "format": options.format,
                            "captured_lines": pane.captured_lines,
                            "omitted_lines": pane.omitted_lines,
                            "text": if destination.is_some() { String::new() } else { pane.text },
                        },
                        "bytes": bytes,
                        "target": target,
                        "source": source,
                        "destination": destination,
                    }),
                    warnings: Vec::new(),
                }
            });
            let _ = sender.send(outcome);
            repaint();
        });
        CommandDispatch::Pending(PendingCommandResult::Outcome(result))
    }
}

/// `[format] [screen|history] [max-lines]`, bounded for a JSON reply or an exported file.
fn capture_options(args: &[String], export: bool) -> CaptureOptions {
    CaptureOptions {
        format: match args.first().map_or("plain", String::as_str) {
            "ansi" => CaptureFormat::Ansi,
            "html" => CaptureFormat::Html,
            _ => CaptureFormat::Plain,
        },
        scope: if args.get(1).is_some_and(|value| value == "history") {
            CaptureScope::History
        } else {
            CaptureScope::Screen
        },
        max_lines: args
            .get(2)
            .and_then(|value| value.parse().ok())
            .unwrap_or(10_000),
        // Worst-case JSON escaping uses six bytes per control character. Leave framing room.
        max_bytes: if export { 2 * 1024 * 1024 } else { 128 * 1024 },
        ..CaptureOptions::default()
    }
}

fn write_capture(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if !path.is_absolute() {
        anyhow::bail!("Export destination must be an absolute local path");
    }
    let parent = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.as_file().sync_all()?;
    // Publishing a complete new file must never replace a user's existing export.
    temporary
        .persist_noclobber(path)
        .map_err(|error| error.error)?;
    Ok(())
}
