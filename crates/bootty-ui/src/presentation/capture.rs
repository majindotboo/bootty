use std::sync::mpsc::{Receiver, TryRecvError};

use bootty_control::{
    Caller, CommandCancellation, CommandInvocation, CommandOutcome, CommandTarget,
};

use crate::gpui::{DialogAction, DialogField, DialogIntent, DialogSpec};

/// The target is captured when the form opens; changing focus cannot redirect an export.
pub struct CaptureDialog {
    target: CommandTarget,
    destination: String,
    format: String,
    scope: String,
    lines: String,
    error: Option<String>,
    saved: Option<String>,
    pending: Option<(Receiver<CommandOutcome>, CommandCancellation)>,
}

pub enum CaptureEvent {
    Close,
    Submit(CommandInvocation),
}

impl CaptureDialog {
    #[must_use]
    pub fn new(target: CommandTarget, destination: String) -> Self {
        Self {
            target,
            destination,
            format: "plain".to_owned(),
            scope: "history".to_owned(),
            lines: "10000".to_owned(),
            error: None,
            saved: None,
            pending: None,
        }
    }

    #[must_use]
    pub fn spec(&self) -> DialogSpec {
        let mut spec = DialogSpec::prompt(
            "terminal-export",
            "Export Terminal",
            &self.destination,
            "Absolute local file path",
            DialogAction::new("export"),
        );
        spec.busy = self.pending.is_some();
        spec.text_label = Some("Destination (new local file)".to_owned());
        spec.fields = [
            ("format", "Format", &self.format),
            ("scope", "Source", &self.scope),
            ("lines", "Maximum lines (latest retained rows)", &self.lines),
        ]
        .into_iter()
        .map(|(id, label, value)| DialogField {
            kind: match id {
                "format" => crate::gpui::DialogFieldKind::Choice(
                    ["plain", "ansi", "html"].map(str::to_owned).to_vec(),
                ),
                "scope" => crate::gpui::DialogFieldKind::Choice(
                    ["screen", "history"].map(str::to_owned).to_vec(),
                ),
                _ => crate::gpui::DialogFieldKind::Text,
            },
            id: id.to_owned(),
            label: label.to_owned(),
            value: value.clone(),
            placeholder: String::new(),
        })
        .collect();
        if let Some(export) = spec.rows.first_mut() {
            (if self.pending.is_some() {
                "Exporting…"
            } else {
                "Export"
            })
            .clone_into(&mut export.label);
            export.enabled = self.pending.is_none()
                && !self.destination.trim().is_empty()
                && self.saved.is_none();
            export.detail.clone_from(&self.error);
        }
        spec.footer = Some(self.saved.clone().unwrap_or_else(|| "Exports rendered terminal state. Existing files are never replaced. tmux and Herdr exports contain their attached client view.".to_owned()));
        spec
    }

    pub fn apply(&mut self, intent: &DialogIntent) -> Option<CaptureEvent> {
        match intent {
            DialogIntent::Dismiss { dialog } if dialog.0 == "terminal-export" => {
                return Some(CaptureEvent::Close);
            }
            DialogIntent::TextChanged { dialog, value }
                if dialog.0 == "terminal-export" && self.pending.is_none() =>
            {
                self.destination.clone_from(value);
            }
            DialogIntent::FieldChanged {
                dialog,
                field,
                value,
            } if dialog.0 == "terminal-export" && self.pending.is_none() => match field.as_str() {
                "format" => self.format.clone_from(value),
                "scope" => self.scope.clone_from(value),
                "lines" => self.lines.clone_from(value),
                _ => return None,
            },
            DialogIntent::Activate { dialog, .. }
                if dialog.0 == "terminal-export"
                    && self.pending.is_none()
                    && self.saved.is_none() =>
            {
                if !matches!(self.format.as_str(), "plain" | "ansi" | "html")
                    || !matches!(self.scope.as_str(), "screen" | "history")
                    || !self
                        .lines
                        .parse::<u32>()
                        .is_ok_and(|lines| (1..=100_000).contains(&lines))
                {
                    self.error = Some(
                        "Choose plain, ansi or html; screen or history; and 1–100000 lines."
                            .to_owned(),
                    );
                    return None;
                }
                let mut command =
                    CommandInvocation::from_action("terminal.export", Caller::Internal);
                command.target = Some(self.target.clone());
                command.arguments = vec![
                    self.destination.clone(),
                    self.format.clone(),
                    self.scope.clone(),
                    self.lines.clone(),
                ];
                return Some(CaptureEvent::Submit(command));
            }
            _ => return None,
        }
        self.error = None;
        self.saved = None;
        None
    }

    pub fn started(
        &mut self,
        response: Receiver<CommandOutcome>,
        cancellation: CommandCancellation,
    ) {
        self.pending = Some((response, cancellation));
        self.error = None;
    }

    pub fn failed(&mut self, error: String) {
        self.error = Some(error);
    }

    pub fn poll(&mut self) {
        let Some((receiver, _)) = &self.pending else {
            return;
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => CommandOutcome::Unavailable {
                message: "Export worker stopped".to_owned(),
            },
        };
        self.pending = None;
        match result {
            CommandOutcome::Success { .. } => {
                self.saved = Some(format!("Saved {}", self.destination));
            }
            result => self.error = crate::commands::command_outcome_message(&result),
        }
    }
}

impl Drop for CaptureDialog {
    fn drop(&mut self) {
        if let Some((_, cancellation)) = &self.pending {
            let _ = cancellation.cancel();
        }
    }
}
