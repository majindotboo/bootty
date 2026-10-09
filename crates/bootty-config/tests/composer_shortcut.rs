use bootty_config::config::ComposerSendShortcut;
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
#[case(ComposerSendShortcut::Enter, false, false, false, Some(false))]
#[case(ComposerSendShortcut::Enter, true, true, false, Some(true))]
#[case(ComposerSendShortcut::Enter, false, false, true, None)]
#[case(
    ComposerSendShortcut::ModEnterMultiline,
    false,
    false,
    false,
    Some(false)
)]
#[case(ComposerSendShortcut::ModEnterMultiline, true, false, false, None)]
#[case(
    ComposerSendShortcut::ModEnterMultiline,
    true,
    true,
    false,
    Some(false)
)]
#[case(ComposerSendShortcut::ModEnterMultiline, true, true, true, Some(true))]
#[case(ComposerSendShortcut::ModEnter, false, false, false, None)]
#[case(ComposerSendShortcut::ModEnter, false, true, false, Some(false))]
#[case(ComposerSendShortcut::ModEnter, false, true, true, Some(true))]
fn enter_preserves_newlines_and_follow_up_overrides(
    #[case] shortcut: ComposerSendShortcut,
    #[case] multiline: bool,
    #[case] secondary: bool,
    #[case] shift: bool,
    #[case] expected: Option<bool>,
) {
    assert_eq!(shortcut.submission(multiline, secondary, shift), expected);
}
