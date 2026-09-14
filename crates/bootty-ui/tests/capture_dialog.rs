#![cfg(test)]

use bootty_control::{CommandCancellation, CommandOutcome, CommandTarget, ResourceKind};
use bootty_ui::{
    gpui::{DialogId, DialogIntent},
    presentation::capture::{CaptureDialog, CaptureEvent},
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn target() -> CommandTarget {
    CommandTarget {
        kind: ResourceKind::Terminal,
        handle: "original-pane".to_owned(),
        generation: 7,
    }
}

#[rstest]
fn export_form_submits_captured_target_and_retains_values_after_failure() {
    let mut dialog = CaptureDialog::new(target(), "/tmp/export.txt".to_owned());
    let id = DialogId::new("terminal-export");
    dialog.apply(&DialogIntent::FieldChanged {
        dialog: id.clone(),
        field: "format".to_owned(),
        value: "ansi".to_owned(),
    });
    let spec = dialog.spec();
    let row = &spec.rows[0];
    let action = row.action.as_ref().unwrap();
    let Some(CaptureEvent::Submit(command)) = dialog.apply(&DialogIntent::Activate {
        dialog: id,
        row: row.id.clone(),
        action: action.id.clone(),
        payload: action.payload.clone(),
    }) else {
        panic!("export command");
    };
    assert_eq!(command.command, "terminal.export");
    assert_eq!(command.target, Some(target()));
    assert_eq!(
        command.arguments,
        ["/tmp/export.txt", "ansi", "history", "10000"]
    );
    let (sender, response) = std::sync::mpsc::channel();
    let cancellation = CommandCancellation::new();
    dialog.started(response, cancellation);
    assert!(!dialog.spec().rows[0].enabled);
    sender
        .send(CommandOutcome::Failed {
            code: "exists".to_owned(),
            message: "Destination exists".to_owned(),
        })
        .unwrap();
    dialog.poll();
    let spec = dialog.spec();
    assert_eq!(spec.text.as_deref(), Some("/tmp/export.txt"));
    assert_eq!(spec.fields[0].value, "ansi");
    assert_eq!(spec.rows[0].detail.as_deref(), Some("Destination exists"));
    assert!(spec.rows[0].enabled);
}

#[rstest]
fn closing_pending_export_cancels_the_shared_command() {
    let mut dialog = CaptureDialog::new(target(), "/tmp/export.txt".to_owned());
    let (_sender, response) = std::sync::mpsc::channel();
    let cancellation = CommandCancellation::new();
    dialog.started(response, cancellation.clone());
    drop(dialog);
    assert!(cancellation.is_cancelled());
}
