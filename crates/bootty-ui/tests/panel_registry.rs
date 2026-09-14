#![cfg(test)]

use std::collections::HashSet;

use bootty_ui::commands::{DockAction, PANELS, PanelCreation};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn panel_registry_is_the_complete_singleton_command_catalog() {
    let names = PANELS
        .iter()
        .map(|panel| panel.name)
        .collect::<HashSet<_>>();
    assert_eq!(names.len(), PANELS.len(), "panel names must be unique");

    let registered_actions = PANELS
        .iter()
        .filter_map(|panel| match panel.creation {
            PanelCreation::Command(action) => Some(action),
            PanelCreation::Context => None,
        })
        .collect::<HashSet<_>>();
    assert_eq!(
        registered_actions,
        DockAction::PANELS.into_iter().collect::<HashSet<_>>()
    );

    let contextual = PANELS
        .iter()
        .filter(|panel| panel.creation == PanelCreation::Context)
        .map(|panel| panel.name)
        .collect::<Vec<_>>();
    assert_eq!(contextual, vec!["bootty.terminal", "bootty.document"]);
}
