#![cfg(test)]

use bootty_ui::composer_commands::ComposerCommand;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn command() -> impl Strategy<Value = ComposerCommand> {
    proptest::sample::select(ComposerCommand::ALL.to_vec())
}

proptest! {
    /// Every built-in answers to the label it is offered under, and to `bootty:NAME`, whatever
    /// names the provider claims.
    #[test]
    fn offered_labels_run_their_command(command in command(), claimed in any::<[bool; 6]>()) {
        let provider_has = |name: &str| {
            ComposerCommand::ALL
                .iter()
                .zip(claimed)
                .any(|(other, claimed)| claimed && other.name() == name)
        };
        let offered = format!("/{}", command.label(provider_has));
        let prefixed = format!("/bootty:{}", command.name());
        prop_assert_eq!(ComposerCommand::parse(&offered, provider_has), Some((command, "")));
        prop_assert_eq!(ComposerCommand::parse(&prefixed, provider_has), Some((command, "")));
    }
}

#[rstest]
#[case("  /rename  Better title  ", Some((ComposerCommand::Rename, "Better title")))]
#[case("/stop", Some((ComposerCommand::Stop, "")))]
// A provider command keeps its name; the built-in moves to `bootty:model`.
#[case("/model", None)]
#[case("/bootty:model", Some((ComposerCommand::Model, "")))]
#[case("/review", None)]
#[case("please /stop", None)]
#[case("/", None)]
fn drafts_name_a_built_in_only_as_a_whole(
    #[case] draft: &str,
    #[case] expected: Option<(ComposerCommand, &str)>,
) {
    assert_eq!(
        ComposerCommand::parse(draft, |name| name == "model"),
        expected
    );
}
