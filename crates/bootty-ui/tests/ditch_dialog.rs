#![cfg(test)]

use bootty_ui::gpui::DialogIntent;
use bootty_ui::presentation::dialogs::{DitchAction, DitchSessionDialog, DitchSessionEvent};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(None)]
#[case(Some(env!("CARGO_MANIFEST_DIR").to_owned()))]
fn remote_ditch_never_offers_local_git_cleanup(#[case] cwd: Option<String>) {
    // A real local repository path must still be treated as remote data.
    let mut dialog = DitchSessionDialog::open_remote("remote-session".to_owned(), cwd.clone());
    let spec = dialog.spec();
    let actions = spec
        .rows
        .iter()
        .filter(|row| row.action.is_some())
        .collect::<Vec<_>>();
    assert_eq!(actions.len(), 1);
    let row = actions[0];
    let action = row.action.clone().expect("kill action");
    assert_eq!(
        dialog.apply(&DialogIntent::Activate {
            dialog: spec.id,
            row: row.id.clone(),
            action: action.id,
            payload: action.payload,
        }),
        Some(DitchSessionEvent::Ditch {
            session_id: "remote-session".to_owned(),
            cwd,
            action: DitchAction::KillOnly,
        })
    );
}
