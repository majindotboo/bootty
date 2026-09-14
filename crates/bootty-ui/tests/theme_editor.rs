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
