use std::{io::Write as _, path::Path, sync::mpsc, time::Instant};

use bootty_control::{CommandCancellation, CommandOutcome, CommandTarget};
use bootty_mux::{executor, target::ExactMuxTarget};
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
        &self,
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
        let Some(binding) = self.workspace.binding(exact.scope()) else {
            return CommandDispatch::Complete(failure("Capture binding was closed"));
        };
        let host = binding
            .multiplexer()
            .remote
            .as_ref()
            .map_or_else(|| "Local".to_owned(), bootty_mux::RemoteTarget::label);
        let Some(pane) = exact.ids().2.map(str::to_owned) else {
            return CommandDispatch::Complete(failure("Capture requires a pane target"));
        };
        let backend = binding.mux().native_backend();
        let (sender, result) = mpsc::channel();
        let repaint = self.repaint.clone();
        std::thread::spawn(move || {
            let result = backend.request(&bootty_mux::native::protocol::Request::Capture { pane, options }).and_then(|response| match response {
                bootty_mux::native::protocol::Response::Captured(capture) => Ok(capture),
                _ => anyhow::bail!("Native mux returned no capture"),
            }).and_then(|mut capture| {
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
                    "source": {"host": host, "kind": "pane_server_state", "original_output": false},
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
