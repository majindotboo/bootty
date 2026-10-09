#![cfg(test)]

use bootty_config::config::{AppearanceVariant, theme_file::ThemeFile};
use bootty_control::{CommandCancellation, CommandOutcome};
use bootty_ui::{
    gpui::{DialogId, DialogIntent},
    presentation::theme_editor::{ThemeEditorDialog, ThemeEditorEvent},
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn complete(
    editor: &mut ThemeEditorDialog,
    action: &str,
    outcome: CommandOutcome,
) -> Option<ThemeEditorEvent> {
    let (sender, receiver) = std::sync::mpsc::channel();
    editor.started(action.to_owned(), receiver, CommandCancellation::new());
    sender.send(outcome).unwrap();
    editor.poll()
}
fn file(revision: Option<&str>) -> CommandOutcome {
    CommandOutcome::Success { value: serde_json::to_value(ThemeFile { name: "Ocean".to_owned(), source: "[metadata]\nname='Ocean'\nsource='Author'\nlicense='MIT'\n[colors]\nbackground='#123456'\npalette=['#000000','#ffffff']\n".to_owned(), revision: revision.map(str::to_owned) }).unwrap(), warnings: vec![] }
}
fn activate(editor: &mut ThemeEditorDialog, action: &str) -> Option<ThemeEditorEvent> {
    let spec = editor.spec();
    let row = spec
        .rows
        .iter()
        .find(|row| {
            row.action
                .as_ref()
                .is_some_and(|value| value.id.0 == action)
        })
        .unwrap();
    let action = row.action.as_ref().unwrap();
    editor.apply(&DialogIntent::Activate {
        dialog: spec.id,
        row: row.id.clone(),
        action: action.id.clone(),
        payload: action.payload.clone(),
    })
}
#[rstest]
#[case(None, "Ocean Copy", 2)]
#[case(Some("revision-one"), "Ocean", 3)]
fn editor_preserves_metadata_and_only_overwrites_loaded_revision(
    #[case] revision: Option<&str>,
    #[case] name: &str,
    #[case] count: usize,
) {
    let mut editor = ThemeEditorDialog::new("Ocean".to_owned(), AppearanceVariant::Dark);
    complete(&mut editor, "theme.read", file(revision));
    assert_eq!(editor.spec().text.as_deref(), Some(name));
    editor.apply(&DialogIntent::FieldChanged {
        dialog: DialogId::new("theme-editor"),
        field: "background".to_owned(),
        value: "#ABCDEF80".to_owned(),
    });
    let Some(ThemeEditorEvent::Submit(command)) = activate(&mut editor, "save") else {
        panic!("save");
    };
    assert_eq!(command.command, "theme.save");
    assert_eq!(command.arguments.len(), count);
    let parsed = bootty_config::config::parse_theme_source(&command.arguments[1], name).unwrap();
    assert_eq!(parsed.info.license, "MIT");
    assert_eq!(parsed.info.source, "Author");
    assert_eq!(parsed.colors.background.unwrap().a, 128);
    editor.apply(&DialogIntent::TextChanged {
        dialog: DialogId::new("theme-editor"),
        value: "Another".to_owned(),
    });
    let Some(ThemeEditorEvent::Submit(command)) = activate(&mut editor, "save") else {
        panic!("copy");
    };
    assert_eq!(command.arguments.len(), 2);
}
#[rstest]
fn save_applies_only_after_success_and_failure_keeps_editable_draft() {
    let mut editor = ThemeEditorDialog::new("Ocean".to_owned(), AppearanceVariant::Light);
    complete(&mut editor, "theme.read", file(None));
    complete(
        &mut editor,
        "theme.save",
        CommandOutcome::Failed {
            code: "conflict".to_owned(),
            message: "Changed on disk".to_owned(),
        },
    );
    assert_eq!(
        editor.spec().rows[0].detail.as_deref(),
        Some("Changed on disk")
    );
    assert!(editor.spec().rows[0].enabled);
    let Some(ThemeEditorEvent::Submit(command)) =
        complete(&mut editor, "theme.save", file(Some("saved")))
    else {
        panic!("apply");
    };
    assert_eq!(command.command, "theme.apply");
    assert_eq!(command.arguments, ["Ocean", "light"]);
}

#[rstest]
#[case("background", "#ABCDEF80")]
#[case("palette-1", "#AABBCC")]
fn native_named_color_edit_previews_the_same_validated_document(
    #[case] field: &str,
    #[case] color: &str,
) {
    let mut editor = ThemeEditorDialog::new("Ocean".to_owned(), AppearanceVariant::Dark);
    complete(&mut editor, "theme.read", file(Some("revision-one")));
    let Some(ThemeEditorEvent::Submit(command)) = editor.edit_field(field, color.to_owned()) else {
        panic!("valid color edit must request a preview");
    };
    assert_eq!(command.command, "theme.preview");
    assert_eq!(command.arguments.get(1).map(String::as_str), Some("dark"));
    let parsed =
        bootty_config::config::parse_theme_source(command.arguments.first().unwrap(), "Ocean")
            .unwrap();
    assert_eq!(parsed.info.source, "Author");
    assert_eq!(parsed.info.license, "MIT");
    let expected = bootty_config::color::Color::from_hex(color).unwrap();
    if field == "background" {
        assert_eq!(parsed.colors.background, Some(expected));
    } else {
        assert_eq!(parsed.colors.palette.get(1), Some(&expected));
    }
    let Some(ThemeEditorEvent::Submit(save)) = editor.activate("save") else {
        panic!("save current draft");
    };
    assert_eq!(
        save.arguments.get(2).map(String::as_str),
        Some("revision-one")
    );
}

#[rstest]
#[case(false)]
#[case(true)]
fn leaving_native_editor_restores_even_a_preview_with_an_unconsumed_reply(#[case] consumed: bool) {
    let mut editor = ThemeEditorDialog::new("Ocean".to_owned(), AppearanceVariant::Light);
    complete(&mut editor, "theme.read", file(None));
    let (sender, receiver) = std::sync::mpsc::channel();
    let cancellation = CommandCancellation::new();
    editor.started("theme.preview".to_owned(), receiver, cancellation);
    sender.send(CommandOutcome::success()).unwrap();
    if consumed {
        editor.poll();
        assert!(editor.preview_active());
    }
    let Some(ThemeEditorEvent::Submit(restore)) = editor.restore_preview() else {
        panic!("restore pending or applied preview");
    };
    assert_eq!(restore.command, "theme.restore");
    assert_eq!(restore.arguments, Vec::<String>::new());
    assert!(editor.preview_active());
    let (_sender, receiver) = std::sync::mpsc::channel();
    editor.started(restore.command, receiver, CommandCancellation::new());
    assert!(editor.restore_preview().is_none());
    complete(&mut editor, "theme.restore", CommandOutcome::success());
    assert!(!editor.preview_active());
    assert!(editor.restore_preview().is_none());
}

#[rstest]
#[case(CommandOutcome::Failed { code: "restore_failed".to_owned(), message: "Restore failed".to_owned() })]
#[case(CommandOutcome::Unavailable { message: "Owner unavailable".to_owned() })]
fn failed_restore_keeps_preview_and_can_retry_after_queue_rejection(
    #[case] restore_failure: CommandOutcome,
) {
    use bootty_control::{AppCommandSendError, Caller, CommandInvocation, app_command_channel};
    use std::{
        sync::Arc,
        time::{Duration, Instant},
    };

    let mut editor = ThemeEditorDialog::new("Ocean".to_owned(), AppearanceVariant::Light);
    complete(&mut editor, "theme.preview", CommandOutcome::success());
    let (sender, requests) = app_command_channel(1, Arc::new(|| {}));
    let sender = sender.for_caller(Caller::Internal);
    let now = Instant::now();
    let deadline = now.checked_add(Duration::from_secs(30)).unwrap();
    let _occupied = sender
        .submit(
            CommandInvocation::from_action("theme.read", Caller::Internal),
            deadline,
            CommandCancellation::new(),
        )
        .unwrap();
    let Some(ThemeEditorEvent::Submit(restore)) = editor.restore_preview() else {
        panic!("Applied preview requires restoration");
    };
    let Err(error) = sender.submit(restore, deadline, CommandCancellation::new()) else {
        panic!("The occupied owner queue must reject restoration");
    };
    assert_eq!(error, AppCommandSendError::Overloaded);
    editor.failed(format!("Theme command could not be queued: {error:?}"));
    assert!(editor.preview_active());
    assert!(editor.spec().rows.first().unwrap().detail.is_some());
    let occupied = requests.try_recv().unwrap();
    assert_eq!(occupied.invocation.command, "theme.read");

    let Some(ThemeEditorEvent::Submit(retry)) = editor.restore_preview() else {
        panic!("Queue rejection must leave restoration available");
    };
    assert_eq!(retry.command, "theme.restore");
    let response = sender
        .submit(retry, deadline, CommandCancellation::new())
        .unwrap();
    editor.started(
        "theme.restore".to_owned(),
        response,
        CommandCancellation::new(),
    );
    assert!(editor.restore_preview().is_none());
    requests
        .try_recv()
        .unwrap()
        .response
        .send(restore_failure)
        .unwrap();
    editor.poll();
    assert!(editor.preview_active());
    assert!(editor.spec().rows.first().unwrap().detail.is_some());
    let Some(ThemeEditorEvent::Submit(retry)) = editor.restore_preview() else {
        panic!("A failed owner outcome must leave restoration available");
    };
    assert_eq!(retry.command, "theme.restore");
    complete(&mut editor, "theme.restore", CommandOutcome::success());
    assert!(!editor.preview_active());
    assert!(editor.restore_preview().is_none());
}
