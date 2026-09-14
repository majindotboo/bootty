use assert_fs::prelude::*;
use bootty_config::{FontStyleAssignment, config::load_config_from_path};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

#[rstest]
#[case("\"auto\"", FontStyleAssignment::Automatic)]
#[case("false", FontStyleAssignment::Disabled)]
#[case("\"SemiBold\"", FontStyleAssignment::Named("SemiBold".to_owned()))]
fn style_assignments_load_for_terminal_and_ui(
    #[case] value: &str,
    #[case] expected: FontStyleAssignment,
) {
    let file = assert_fs::NamedTempFile::new("config.toml").unwrap();
    file.write_str(&format!("[font]\nstyle-bold = {value}\nstyle-italic = {value}\nstyle-bold-italic = {value}\n[font.ui-weights]\nbold = {value}\n")).unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    assert_eq!(config.font.style_bold, expected);
    assert_eq!(config.font.style_italic, expected);
    assert_eq!(config.font.style_bold_italic, expected);
    assert_eq!(
        config
            .font
            .ui_weights
            .get(&bootty_config::FontWeightRole::Bold),
        Some(&expected)
    );
}

#[rstest]
#[case("true")]
#[case("700")]
#[case("[]")]
fn invalid_style_assignment_is_rejected(#[case] value: &str) {
    let file = assert_fs::NamedTempFile::new("config.toml").unwrap();
    file.write_str(&format!("[font]\nstyle-bold = {value}\n"))
        .unwrap();
    assert!(load_config_from_path(file.path()).is_err());
}

proptest! {
    #[test]
    fn named_styles_preserve_the_advertised_name(name in "[A-Z][a-zA-Z0-9 -]{0,40}") {
        let expected = FontStyleAssignment::Named(name);
        let json = serde_json::to_string(&expected).unwrap();
        prop_assert_eq!(serde_json::from_str::<FontStyleAssignment>(&json).unwrap(), expected);
    }
}
