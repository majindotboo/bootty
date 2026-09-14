#![cfg(test)]

use bootty_ui::gpui::keybinding_contexts_overlap;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case("Global", "Command", true)]
#[case("Workspace", "Sidebar", true)]
#[case("Terminal", "Native", true)]
#[case("terminal", "rmux", true)]
#[case("Terminal", "Terminal && backend == tmux", true)]
#[case("Command", "Command && mode == picker", true)]
#[case("rmux", "tmux", false)]
#[case("Sidebar", "Terminal", false)]
fn editor_conflicts_share_scope_semantics(
    #[case] left: &str,
    #[case] right: &str,
    #[case] expected: bool,
) {
    assert_eq!(keybinding_contexts_overlap(left, right), expected);
    assert_eq!(keybinding_contexts_overlap(right, left), expected);
}
