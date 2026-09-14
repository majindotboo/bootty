//! Every built-in setting spec must describe a key the loader actually reads.
//!
//! Paths used to be bare string literals at the call site with nothing checking them. These tests
//! are what replaces that: a wrong path, a kebab/underscore typo, a fallback that drifted from
//! `defaults.rs`, or a choice token the parser rejects all fail here.
use assert_fs::prelude::*;
use bootty_config::config::{BoottyConfig, load_config_from_path};
use bootty_config::settings_schema::{SettingKind, SettingSpec, SettingValue, SettingsSchema};
use pretty_assertions::{assert_eq, assert_ne};

#[rstest::rstest]
#[case("auto", bootty_config::config::TerminalScrollbar::Auto)]
#[case("hover", bootty_config::config::TerminalScrollbar::Hover)]
#[case("always", bootty_config::config::TerminalScrollbar::Always)]
#[case("never", bootty_config::config::TerminalScrollbar::Never)]
fn terminal_scrollbar_modes_load_and_default_to_auto(
    #[case] token: &str,
    #[case] expected: bootty_config::config::TerminalScrollbar,
) {
    assert_eq!(
        BoottyConfig::default().session.scrollbar,
        bootty_config::config::TerminalScrollbar::Auto
    );
    assert_eq!(
        load_with(&["session", "scrollbar"], &format!("\"{token}\""))
            .expect("valid config")
            .session
            .scrollbar,
        expected
    );
}

/// Write one key into an otherwise empty config and load it back.
fn load_with(path: &[&str], toml_value: &str) -> Result<BoottyConfig, Box<dyn std::error::Error>> {
    let directory = assert_fs::TempDir::new()?;
    let config_path = directory.child("config.toml");
    let (leaf, parents) = path.split_last().ok_or("non-empty path")?;
    let source = if parents.is_empty() {
        format!("{leaf} = {toml_value}\n")
    } else {
        format!("[{}]\n{leaf} = {toml_value}\n", parents.join("."))
    };
    config_path.write_str(&source)?;
    load_config_from_path(config_path.path())
        .map_err(|error| format!("{source}\nfailed to load: {error}").into())
}

#[test]
fn every_spec_default_matches_the_config_default() {
    let defaults = BoottyConfig::default();
    for spec in SettingsSchema::builtin().specs() {
        if matches!(&spec.kind, SettingKind::Custom(_)) {
            assert_eq!(spec.default_value(&defaults), None);
            continue;
        }
        let value = spec.default_value(&defaults).expect("scalar default");
        let matches_kind = matches!(
            (&spec.kind, &value),
            (SettingKind::Bool, SettingValue::Bool(_))
                | (SettingKind::Text { .. }, SettingValue::Text(_))
                | (SettingKind::Number { .. }, SettingValue::Number(_))
                | (SettingKind::Choice { .. }, SettingValue::Token(_))
                | (
                    SettingKind::FontStyle,
                    SettingValue::Token(_) | SettingValue::Text(_) | SettingValue::Bool(false)
                )
                | (SettingKind::Custom(_), _)
        );
        assert!(
            matches_kind,
            "{}: default {value:?} does not match its kind",
            spec.id()
        );
        if let (SettingKind::Number { range, .. }, SettingValue::Number(number)) =
            (&spec.kind, &value)
        {
            assert!(
                range.contains(number),
                "{}: default {number} is outside {range:?}",
                spec.id()
            );
        }
        if let (SettingKind::Choice { options }, SettingValue::Token(token)) = (&spec.kind, &value)
        {
            for option in options {
                assert!(
                    option
                        .description
                        .as_ref()
                        .is_none_or(|text| !text.trim().is_empty()),
                    "{}: {} must omit an absent description instead of rendering a blank line",
                    spec.id(),
                    option.token,
                );
            }
            assert!(
                options.iter().any(|option| option.token == *token),
                "{}: default token {token:?} is not one of its options",
                spec.id()
            );
        }
    }
}

#[test]
fn cursor_blink_is_a_typed_boolean_setting() {
    let schema = SettingsSchema::builtin();
    let spec = schema
        .get("cursor.blink")
        .expect("cursor blink is declared in the settings schema");

    assert!(matches!(&spec.kind, SettingKind::Bool));
    assert_eq!(
        spec.default_value(&BoottyConfig::default())
            .expect("scalar default"),
        SettingValue::Bool(false)
    );
    assert_eq!(
        spec.default_value(&load_with(&["cursor", "blink"], "true").expect("valid config"))
            .expect("scalar default"),
        SettingValue::Bool(true)
    );
}

#[test]
fn every_spec_path_round_trips_through_the_loader() {
    for spec in SettingsSchema::builtin().specs() {
        if matches!(&spec.kind, SettingKind::Custom(_)) {
            continue;
        }
        let path = spec.path_parts();
        let default = spec
            .default_value(&BoottyConfig::default())
            .expect("scalar default");
        let (written, expected) = match &spec.kind {
            SettingKind::Bool => {
                // Probe the opposite of the default, or the write could land nowhere unnoticed.
                let probe = !default.as_bool().unwrap_or_default();
                (probe.to_string(), SettingValue::Bool(probe))
            }
            SettingKind::Text { .. }
                if matches!(
                    spec.id().as_str(),
                    "window.background-gradient-start" | "window.background-gradient-end"
                ) =>
            {
                (
                    "\"#12345678\"".to_owned(),
                    SettingValue::Text("#12345678".to_owned()),
                )
            }
            SettingKind::Text { .. } => (
                "\"round trip\"".to_owned(),
                SettingValue::Text("round trip".to_owned()),
            ),
            SettingKind::Number { range, .. } => {
                // Pick a value inside the range that is not the default, so a path that silently
                // writes nowhere cannot pass by reading the default back.
                let midpoint = ((range.start() + range.end()) / 2.0 * 10.0).round() / 10.0;
                let default = default
                    .as_number()
                    .expect("number setting has number default");
                let value = [midpoint, *range.start(), *range.end()]
                    .into_iter()
                    .find(|candidate| candidate.to_bits() != default.to_bits())
                    .expect("number range contains a non-default probe");
                (format!("{value}"), SettingValue::Number(value))
            }
            SettingKind::Choice { options } => {
                let option = options
                    .iter()
                    .find(|option| SettingValue::Token(option.token.to_string()) != default)
                    .expect("a choice has a non-default option");
                (
                    format!("\"{}\"", option.token),
                    SettingValue::Token(option.token.to_string()),
                )
            }
            SettingKind::FontStyle => ("false".to_owned(), SettingValue::Bool(false)),
            SettingKind::Custom(_) => continue,
        };

        // A value equal to the default would let a path that writes nowhere pass.
        assert_ne!(
            expected,
            default,
            "{}: the probe value must differ from the default",
            spec.id()
        );

        let config = load_with(&path, &written).expect("valid config");
        let read_back = spec.default_value(&config).expect("scalar default");
        assert_eq!(
            read_back,
            expected,
            "{}: writing {written} at {path:?} did not read back",
            spec.id()
        );
    }
}

#[test]
fn spec_ids_are_unique_and_resolvable() {
    let schema = SettingsSchema::builtin();
    for spec in schema.specs() {
        assert!(
            schema.get(&spec.id()).is_some(),
            "{} is not resolvable by id",
            spec.id()
        );
    }
    let mut ids: Vec<String> = schema.specs().iter().map(SettingSpec::id).collect();
    ids.sort();
    let count = ids.len();
    ids.dedup();
    assert_eq!(ids.len(), count, "duplicate setting ids");
}

#[test]
fn every_hand_written_spec_has_a_known_editor_owner() {
    for spec in SettingsSchema::builtin().specs() {
        if let SettingKind::Custom(editor) = &spec.kind {
            assert_eq!(
                spec.page,
                editor.name(),
                "{} has a mismatched owner",
                spec.id()
            );
        }
    }
}

#[test]
fn an_unregistered_toml_leaf_fails_at_the_schema_boundary() {
    let directory = assert_fs::TempDir::new().expect("temporary config directory");
    let config_path = directory.child("config.toml");
    config_path
        .write_str("[window]\nsetting-that-has-no-editor = true\n")
        .expect("write config");

    let error = load_config_from_path(config_path.path()).expect_err("unknown setting must fail");
    assert!(
        error
            .to_string()
            .contains("unsupported config setting window.setting-that-has-no-editor")
    );
    assert!(error.to_string().contains("declare it in SettingsSchema"));
}

#[test]
fn builtin_schema_keeps_the_hand_written_declarations() {
    let schema = SettingsSchema::new(SettingsSchema::builtin().specs().to_vec());
    assert!(schema.allows_path(&["cursor", "style"]));
    assert!(schema.allows_path(&["cursor", "dim-inactive-pane"]));
    assert!(schema.allows_path(&["input", "copy-on-select"]));
    assert!(schema.allows_path(&["multiplexer", "remote", "args"]));
    assert_eq!(schema.get("cursor.style").unwrap().page, "appearance");
    assert_eq!(
        schema
            .get("input.hide-mouse-pointer-while-typing")
            .unwrap()
            .page,
        "appearance"
    );
}

#[rstest::rstest]
fn panel_preferences_round_trip_through_the_schema(
    #[values("left", "right", "bottom")] dock: &str,
    #[values("none", "top", "bottom")] button: &str,
) {
    use bootty_config::config::{PanelButton, PanelDock, PanelKind};
    for kind in PanelKind::ALL {
        let directory = assert_fs::TempDir::new().unwrap();
        let file = directory.child("config.toml");
        file.write_str(&format!(
            "[panels.{}]\ndock = {dock:?}\nbutton = {button:?}\n",
            kind.name()
        ))
        .unwrap();
        let config = load_config_from_path(file.path()).unwrap();
        let expected_dock = match dock {
            "left" => PanelDock::Left,
            "bottom" => PanelDock::Bottom,
            _ => PanelDock::Right,
        };
        let expected_button = match button {
            "top" => PanelButton::Top,
            "bottom" => PanelButton::Bottom,
            _ => PanelButton::None,
        };
        assert_eq!(config.panel(kind).dock(kind), expected_dock);
        assert_eq!(config.panel(kind).button, expected_button);
        let defaults = BoottyConfig::default();
        assert_eq!(defaults.panel(kind).button, PanelButton::None);
        assert_eq!(
            defaults.panel(kind).dock(kind),
            if kind == PanelKind::Sessions {
                PanelDock::Left
            } else {
                PanelDock::Right
            }
        );
    }
}

#[rstest::rstest]
fn legacy_sidebar_configuration_remains_loadable_for_layout_migration() {
    let file = assert_fs::NamedTempFile::new("config.toml").unwrap();
    file.write_str(
        "[chrome]\nsidebar = false\nsidebar-width = 410\n[sidebar]\nposition = \"right\"\n",
    )
    .unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    assert!(!config.chrome.sidebar);
    assert!((config.chrome.sidebar_width - 410.0).abs() < f32::EPSILON);
    assert_eq!(
        config.sidebar.position,
        bootty_config::config::SidebarPosition::Right
    );
    let schema = SettingsSchema::builtin();
    for path in [
        ["chrome", "sidebar"],
        ["chrome", "sidebar-width"],
        ["sidebar", "position"],
    ] {
        assert!(schema.allows_path(&path));
        assert!(
            !schema
                .specs()
                .iter()
                .any(|spec| spec.id() == path.join("."))
        );
    }
}

#[rstest::rstest]
#[case("dock-tabs")]
#[case("terminal-tabs")]
fn partial_tab_settings_preserve_surface_defaults(#[case] surface: &str) {
    let file = assert_fs::NamedTempFile::new("config.toml").unwrap();
    file.write_str(&format!("[chrome.{surface}]\nclose-position = \"left\"\n"))
        .unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    let defaults = BoottyConfig::default();
    let (actual, expected) = if surface == "dock-tabs" {
        (config.chrome.dock_tabs, defaults.chrome.dock_tabs)
    } else {
        (config.chrome.terminal_tabs, defaults.chrome.terminal_tabs)
    };
    assert_eq!(
        actual.close_position,
        bootty_config::config::TabClosePosition::Left
    );
    assert_eq!(actual.appearance, expected.appearance);
    assert_eq!(actual.close_button, expected.close_button);
}
