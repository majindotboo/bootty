#![cfg(test)]

use bootty_config::config::{MultiplexerBackendConfig, RemoteConfig, WslDistribution};
use bootty_mux::repository::{SpaceMuxOverride, SpaceRemoteOverride};
use bootty_ui::{
    gpui::SpaceEditorIntent,
    presentation::dialogs::{SpaceEditorDialog, SpaceEditorEvent},
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn wsl_space_selection_preserves_linux_placement_and_limits_backends() {
    let mut dialog =
        SpaceEditorDialog::new_space("terminal".to_owned(), SpaceMuxOverride::default())
            .with_distributions(vec![WslDistribution::new("Ubuntu 開発").unwrap()]);
    dialog.apply(SpaceEditorIntent::SetName("Linux project".to_owned()));
    dialog.apply(SpaceEditorIntent::SelectLocation(
        "wsl:Ubuntu 開発".to_owned(),
    ));
    let snapshot = dialog.snapshot();
    assert!(snapshot.can_save);
    assert_eq!(snapshot.backends.len(), 2);
    dialog.apply(SpaceEditorIntent::SelectBackend(Some("native".to_owned())));
    let Some(SpaceEditorEvent::Save(draft)) = dialog.apply(SpaceEditorIntent::Save) else {
        panic!("save WSL Space");
    };
    assert_eq!(draft.backend, Some(MultiplexerBackendConfig::Rmux));
    let SpaceRemoteOverride::Inline(RemoteConfig::Wsl(remote)) = draft.remote_source else {
        panic!("WSL placement");
    };
    assert_eq!(remote.distribution.as_str(), "Ubuntu 開発");
}
