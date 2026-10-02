#![cfg(test)]

use std::sync::Arc;

use assert_fs::prelude::*;
use bootty_config::{
    config::{BoottyConfig, load_config_from_path, load_or_create_config_document},
    settings_schema::{SettingDefault, SettingValue, SettingsSchema},
};
use bootty_ui::settings_session::{AcceptedSettings, Catalogs, SettingsSession};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn accepted(path: &std::path::Path, revision: u64) -> AcceptedSettings {
    AcceptedSettings {
        revision,
        config: Arc::new(load_config_from_path(path).unwrap()),
        document: load_or_create_config_document(path).unwrap(),
        schema: Arc::new(SettingsSchema::new(
            SettingsSchema::builtin().specs().to_vec(),
        )),
    }
}

fn session(source: &str) -> (assert_fs::TempDir, SettingsSession) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.child("config.toml");
    path.write_str(source).unwrap();
    let accepted = accepted(path.path(), 1);
    (
        directory,
        SettingsSession::new(accepted, Catalogs::default()),
    )
}

#[rstest]
fn explicitly_saved_scalar_defaults_do_not_offer_reset() {
    let (_directory, mut session) = session("");
    let defaults = BoottyConfig::default();
    for spec in SettingsSchema::builtin().specs() {
        if matches!(spec.default, SettingDefault::Unused) {
            continue;
        }
        let id = spec.id();
        assert!(session.can_reset(&id), "{id} retains schema ownership");
        let default = spec.default_value(&defaults).expect("scalar default");
        assert!(session.set_value(&id, &default));
        assert!(session.is_default(&id), "{id}: {default:?}");
    }
}

#[rstest]
#[case("font.size", SettingValue::Number(23.0))]
#[case("font.style-bold", SettingValue::Token("Thin".into()))]
#[case("font.ui-weights.bold", SettingValue::Bool(false))]
#[case("session.term", SettingValue::Text("vt100".into()))]
fn unsaved_changes_and_resets_override_the_accepted_snapshot(
    #[case] id: &str,
    #[case] changed: SettingValue,
) {
    let (_directory, mut session) = session("");
    assert!(session.is_default(id));
    assert!(session.set_value(id, &changed));
    assert!(!session.is_default(id));
    assert!(session.remove_value(id));
    assert!(session.is_default(id));
}

#[rstest]
#[case("font.family")]
#[case("font.ui-family")]
fn explicit_default_font_face_and_family_are_equivalent(#[case] id: &str) {
    let (_directory, mut session) = session("");
    let config = BoottyConfig::default();
    let family = if id == "font.family" {
        config.font.family
    } else {
        config.font.ui_family
    };
    assert!(session.set_string_list(id, &family));
    assert!(session.is_default(id));
    let canonical = gpui_kit::font_name_with_fallbacks(&family[0], &family[0]).to_owned();
    assert!(session.set_string_list(id, std::slice::from_ref(&canonical)));
    assert!(session.is_default(id));
    let database = bootty_ui::font_database::system_font_database();
    let face = database
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(&canonical)],
            ..fontdb::Query::default()
        })
        .unwrap();
    let named = database.face(face).unwrap().post_script_name.clone();
    assert!(session.set_string_list(id, &[named]));
    assert!(session.is_default(id));
    assert!(session.set_string_list(id, &["Test Changed Family".into()]));
    assert!(!session.is_default(id));
}

#[rstest]
fn structured_defaults_compare_actual_values_and_ignore_unrelated_edits() {
    let (_directory, mut session) = session("");
    let defaults = BoottyConfig::default();
    assert!(session.set_status_segments(true, defaults.chrome.top_segments.clone()));
    assert!(session.is_default("chrome.top-segment"));
    assert!(session.set_string_list("sidebar.modules", &defaults.sidebar.modules));
    assert!(session.is_default("sidebar.modules"));
    assert!(session.set_environment(Vec::new()));
    assert!(session.is_default("session.env"));
    assert!(session.set_environment(vec![("HELLO".into(), "world".into())]));
    assert!(!session.is_default("session.env"));
    assert!(session.set_value("font.size", &SettingValue::Number(22.0)));
    assert!(session.is_default("chrome.top-segment"));
    assert!(!session.is_default("session.env"));
}

#[rstest]
fn colors_compare_rgba_values_instead_of_hex_spelling() {
    let (_directory, mut session) = session("");
    let color = BoottyConfig::default()
        .appearance
        .dark
        .colors
        .foreground
        .unwrap();
    let id = "appearance.dark.colors.foreground";
    assert!(session.set_custom_value(
        id,
        &SettingValue::Text(format!(
            "#{:02X}{:02X}{:02X}{:02X}",
            color.r, color.g, color.b, color.a
        ))
    ));
    assert!(session.is_default(id));
    assert!(session.set_custom_value(id, &SettingValue::Text("#123456".into())));
    assert!(!session.is_default(id));
}

#[rstest]
fn resolved_inherited_values_and_aliases_are_used_when_the_root_has_no_value() {
    let directory = assert_fs::TempDir::new().unwrap();
    directory
        .child("included.toml")
        .write_str("[font]\nfamily = ['Lilex-Bold']\nsize = 23\n[chrome]\nstatus-bar = false\n")
        .unwrap();
    let path = directory.child("config.toml");
    path.write_str("include = ['included.toml']\n").unwrap();
    let mut session = SettingsSession::new(accepted(path.path(), 1), Catalogs::default());
    assert!(!session.is_default("font.family"));
    assert!(!session.is_default("font.size"));
    assert_eq!(
        session.is_default("chrome.top-bar"),
        !BoottyConfig::default().chrome.top_bar
    );
    assert!(session.set_value(
        "font.size",
        &SettingValue::Number(BoottyConfig::default().font.size)
    ));
    assert!(session.is_default("font.size"));
    assert!(!session.is_default("font.family"));
}

#[rstest]
fn document_acceptance_updates_resolved_values_without_a_followup_reconciliation() {
    let (directory, mut session) = session("");
    let path = directory.child("config.toml");
    path.write_str("[font]\nfamily = ['Lilex-Bold']\n").unwrap();
    let next = accepted(path.path(), 2);
    session.apply_outcome(
        bootty_ui::settings_session::SettingsOutcome::DocumentAccepted {
            source: bootty_ui::settings_session::SettingsWriteSource::Document,
            accepted: Box::new(next.clone()),
            warning: None,
        },
    );
    assert!(!session.is_default("font.family"));
    session.reconcile_accepted(next);
    assert!(!session.is_default("font.family"));
}

#[rstest]
fn selected_user_theme_is_the_color_reset_baseline() {
    let directory = assert_fs::TempDir::new().unwrap();
    directory
        .child("themes/Custom.toml")
        .write_str("[colors]\nforeground = '#123456'\nbackground = '#234567'\n")
        .unwrap();
    let path = directory.child("config.toml");
    path.write_str("[appearance.dark]\ntheme = 'Custom'\n")
        .unwrap();
    let mut session = SettingsSession::new(accepted(path.path(), 1), Catalogs::default());
    let id = "appearance.dark.colors.foreground";
    assert!(session.is_default(id));
    assert!(session.set_custom_value(id, &SettingValue::Text("#123456ff".into())));
    assert!(session.is_default(id));
    assert!(session.set_custom_value(id, &SettingValue::Text("#ABCDEF".into())));
    assert!(!session.is_default(id));
    assert!(session.remove_custom_value(id));
    assert!(session.is_default(id));

    path.write_str("theme = 'Custom'\n[colors]\nforeground = '#ABCDEF'\n")
        .unwrap();
    let legacy = accepted(path.path(), 2);
    assert_eq!(
        legacy.config.appearance.light.theme_colors.foreground,
        bootty_config::color::Color::from_hex("#123456").ok()
    );
    assert_eq!(
        legacy.config.appearance.light.theme_colors,
        legacy.config.appearance.dark.theme_colors
    );
    let session = SettingsSession::new(legacy, Catalogs::default());
    assert!(!session.is_default("appearance.dark.colors.foreground"));
    assert!(!session.is_default("appearance.light.colors.foreground"));
}

#[rstest]
#[case("font.size", SettingValue::Number(23.0), SettingValue::Number(17.0))]
#[case("session.term", SettingValue::Text("vt100".to_owned()), SettingValue::Text("xterm".to_owned()))]
#[case(
    "session.shell-integration",
    SettingValue::Bool(false),
    SettingValue::Bool(true)
)]
fn scalar_values_follow_accepted_includes_until_locally_edited(
    #[case] id: &str,
    #[case] inherited: SettingValue,
    #[case] edited: SettingValue,
    #[values(false, true)] shadowed_root_values: bool,
) {
    let directory = assert_fs::TempDir::new().unwrap();
    let path = directory.child("config.toml");
    path.write_str(if shadowed_root_values {
        "include = ['included.toml']\n[font]\nsize = 11\n[session]\nterm = 'ansi'\nshell-integration = true\n"
    } else { "include = ['included.toml']\n" }).unwrap();
    directory
        .child("included.toml")
        .write_str("[font]\nsize = 23\n[session]\nterm = 'vt100'\nshell-integration = false\n")
        .unwrap();
    let accepted = accepted(path.path(), 1);
    let default = accepted
        .schema
        .get(id)
        .unwrap()
        .default_value(&BoottyConfig::default())
        .unwrap();
    let mut session = SettingsSession::new(accepted.clone(), Catalogs::default());
    assert_eq!(session.value(id), Some(inherited.clone()));
    session.set_value(id, &edited);
    assert_eq!(session.value(id), Some(edited));
    session.remove_value(id);
    assert_eq!(session.value(id), Some(default));
    session.discard_document_changes(accepted);
    assert_eq!(session.value(id), Some(inherited));
}
