use std::{io::Write as _, path::Path, sync::mpsc, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome, CommandTarget, ResourceKind};
use bootty_mux::{
    backend::PaneCapture, executor, target::ExactMuxTarget, terminal::TerminalRuntime,
};
use bootty_terminal::terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope};

use super::{CommandDispatch, PendingCommandResult};
use crate::AppState;

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
        let pending = match terminal.capture(options) {
            Ok(pending) => pending,
            Err(error) => return CommandDispatch::Complete(failure(error.to_string())),
        };
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = pending.receive("capturing terminal").and_then(|result| result.map_err(anyhow::Error::msg)).and_then(|mut capture| {
                // Capture itself is read-only. Claim the mutation only when ready to publish,
                // allowing a dismissed form or timed-out caller to cancel while formatting.
                executor::begin_synchronous_command(Some((deadline, cancellation))).map_err(|error| anyhow::anyhow!("Capture stopped before export: {error:?}"))?;
                let bytes = capture.text.len();
                if let Some(path) = &destination {
                    write_capture(Path::new(path), capture.text.as_bytes())?;
                    capture.text.clear();
                }
                Ok(serde_json::json!({
                    "capture": capture,
                    "bytes": bytes,
                    "target": target,
                    "source": {"host": host, "backend": backend, "kind": if native { "pane_render_state" } else { "client_attachment_render_state" }, "original_output": false},
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
