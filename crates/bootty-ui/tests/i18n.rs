#![cfg(test)]

use bootty_ui::{
    gpui::{
        ScalarValue, SettingsCategory, SettingsChoice, SettingsControl, SettingsPage,
        SettingsPageItem, SettingsRow,
    },
    i18n::{Localizer, localize_settings, presentation_key},
};
use fluent_bundle::FluentArgs;
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;

fn without_isolation(text: &str) -> String {
    text.replace(['\u{2068}', '\u{2069}'], "")
}

#[rstest]
#[case(0, "Completed in 0 seconds")]
#[case(1, "Completed in 1 second")]
#[case(2, "Completed in 2 seconds")]
fn english_fallback_uses_english_plurals(#[case] seconds: u32, #[case] expected: &str) {
    let localizer = Localizer::new("ja-JP").unwrap();
    let mut args = FluentArgs::new();
    args.set("seconds", seconds);
    assert_eq!(
        without_isolation(&localizer.message("command-duration", Some(&args))),
        expected
    );
    assert_eq!(localizer.message("common-save", None), "Save");
}

#[rstest]
#[case(1, "one")]
#[case(2, "few")]
#[case(5, "many")]
#[case(21, "one")]
fn translations_own_their_plural_rules(#[case] seconds: u32, #[case] expected: &str) {
    let translated = "command-duration = { $seconds ->\n    [one] one\n    [few] few\n    [many] many\n   *[other] other\n    }\n";
    let localizer = Localizer::with_translation("ru", Some(translated)).unwrap();
    let mut args = FluentArgs::new();
    args.set("seconds", seconds);
    assert_eq!(localizer.message("command-duration", Some(&args)), expected);
}

#[rstest]
fn a_bad_translated_parameter_falls_back_without_hiding_other_translations() {
    let localizer = Localizer::with_translation(
        "fr",
        Some("common-save = Enregistrer\ncommand-finished-status = { $typo }\n"),
    )
    .unwrap();
    let mut args = FluentArgs::new();
    args.set("status", 7);
    assert_eq!(
        without_isolation(&localizer.message("command-finished-status", Some(&args))),
        "Command finished with exit status 7"
    );
    assert_eq!(localizer.message("common-save", None), "Enregistrer");
    assert!(Localizer::with_translation("fr", Some("= broken")).is_err());
}

proptest! {
    #[test]
    fn pseudo_locale_keeps_inserted_user_data_intact(name in "[^\\p{C}]{1,100}") {
        let mut args = FluentArgs::new();
        args.set("name", name.as_str());
        let translated = Localizer::new("en-XA").unwrap().message("document-unsaved", Some(&args));
        prop_assert!(translated.contains(&name));
        prop_assert!(translated.contains('［'));
        prop_assert!(translated.len() > name.len());
    }
}

#[rstest]
fn settings_translation_keeps_machine_contracts_and_user_values() {
    let mut pages = vec![SettingsPage {
        category: SettingsCategory::General,
        title: "General".into(),
        search_terms: "startup".into(),
        items: vec![
            SettingsPageItem::Setting(SettingsRow::Value {
                id: "session.bell".into(),
                label: "Bell".into(),
                help: "Choose a bell".into(),
                value: ScalarValue::Token("audio".into()),
                control: SettingsControl::Choice(vec![SettingsChoice {
                    token: "audio".into(),
                    label: "Audio".into(),
                    description: Some("Play sound".into()),
                }]),
                enabled: true,
            }),
            SettingsPageItem::Setting(SettingsRow::Value {
                id: "session.working-directory".into(),
                label: "Working directory".into(),
                help: "Initial directory".into(),
                value: ScalarValue::Text("/tmp/été.txt".into()),
                control: SettingsControl::ReadOnly,
                enabled: true,
            }),
        ],
    }];
    localize_settings(&mut pages, &Localizer::new("en-XA").unwrap());
    assert!(pages[0].title.starts_with('［'));
    assert!(pages[0].search_terms.contains("startup"));
    let SettingsPageItem::Setting(SettingsRow::Value {
        id,
        label,
        value,
        control: SettingsControl::Choice(choices),
        ..
    }) = &pages[0].items[0]
    else {
        panic!("choice row")
    };
    assert_eq!(id, "session.bell");
    assert_eq!(value, &ScalarValue::Token("audio".into()));
    assert_ne!(label, "Bell");
    assert_eq!(choices[0].token, "audio");
    assert_ne!(choices[0].label, "Audio");
    let SettingsPageItem::Setting(SettingsRow::Value { value, .. }) = &pages[0].items[1] else {
        panic!("path row")
    };
    assert_eq!(value, &ScalarValue::Text("/tmp/été.txt".into()));
    assert_eq!(
        presentation_key("setting", "session.bell", "label"),
        "setting-session--bell-label"
    );
}

#[rstest]
fn palette_translates_before_filtering_and_retains_action_search() {
    use bootty_ui::{
        gpui::{DialogId, DialogIntent},
        presentation::dialogs::{CommandPaletteDialog, CommandPaletteState},
    };
    let localizer = Localizer::with_translation(
        "fr",
        Some("command-new_mux_session-title = Nouvelle session\n"),
    )
    .unwrap();
    let mut palette =
        CommandPaletteDialog::open_localized(&[], CommandPaletteState::default(), &localizer);
    palette.apply(&DialogIntent::TextChanged {
        dialog: DialogId::new("command-palette"),
        value: "Nouvelle".into(),
    });
    assert_eq!(palette.current_action(), Some("new_mux_session"));
    palette.apply(&DialogIntent::TextChanged {
        dialog: DialogId::new("command-palette"),
        value: "new_mux_session".into(),
    });
    assert_eq!(palette.current_action(), Some("new_mux_session"));
    assert!(
        palette
            .spec()
            .rows
            .iter()
            .any(|row| row.label == "Nouvelle session")
    );
}
