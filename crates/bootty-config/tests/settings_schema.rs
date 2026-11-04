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

/// Files written for removed features keep loading: each retired key is dropped with a warning,
/// and naming the native backend is not a retired choice.
#[rstest::rstest]
#[case::agents("[agents.codex]\nenabled = true\n", Some("agents"))]
#[case::browser("[browser]\nsearch-engine = \"google\"\n", Some("browser"))]
#[case::computer("[computer]\nenabled = true\n", Some("computer"))]
#[case::tray("[tray]\nvisibility = \"always\"\n", Some("tray"))]
#[case::panels("[panels.files]\ndock = \"right\"\n", Some("panels"))]
#[case::right_dock(
    "[chrome]\nright-dock-toggle = false\n",
    Some("chrome.right-dock-toggle")
)]
#[case::agent_notifications(
    "[session]\nagent-notifications = \"always\"\n",
    Some("session.agent-notifications")
)]
#[case::external_backend("[multiplexer]\nbackend = \"rmux\"\n", Some("multiplexer.backend"))]
#[case::tmux_status(
    "[multiplexer]\nhide-tmux-status = true\n",
    Some("multiplexer.hide-tmux-status")
)]
#[case::backend_keybind(
    "[input.backend-keybind]\ntmux = [\"ctrl+a=new_tab\"]\n",
    Some("input.backend-keybind.tmux")
)]
#[case::working_animation(
    "[sidebar]\nanimate-working = false\n",
    Some("sidebar.animate-working")
)]
#[case::native_backend("[multiplexer]\nbackend = \"native\"\n", None)]
fn retired_settings_load_with_a_warning(#[case] source: &str, #[case] retired: Option<&str>) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("config.toml");
    file.write_str(source).unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    assert_eq!(
        config.compatibility_warnings,
        retired
            .map(|path| format!("retired setting ignored: {path}"))
            .into_iter()
            .collect::<Vec<_>>()
    );
}

/// A TOML binding to a removed action would reject the whole config; it is dropped with a
/// warning and the other bindings keep working.
#[rstest::rstest]
#[case::keybind("keybind")]
#[case::sidebar("sidebar-keybind")]
fn retired_keybind_actions_load_with_a_warning(#[case] key: &str) {
    let directory = assert_fs::TempDir::new().unwrap();
    let file = directory.child("config.toml");
    file.write_str(&format!(
        "[input]\n{key} = [\"cmd+1=toggle_right_dock\", \"cmd+2=show_files:1\", \"cmd+3=new_tab\"]\n"
    ))
    .unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    assert_eq!(
        config.compatibility_warnings,
        [
            format!("retired keybind action ignored in input.{key}: toggle_right_dock"),
            format!("retired keybind action ignored in input.{key}: show_files"),
        ]
    );
    let entries = if key == "keybind" {
        &config.input.keybind
    } else {
        &config.input.sidebar_keybind
    };
    assert!(
        entries.iter().any(|entry| entry == "cmd+3=new_tab"),
        "{entries:?}"
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry.contains("toggle_right_dock") || entry.contains("show_files")),
        "{entries:?}"
    );
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
#[case("tabs")]
#[case("dock-tabs")]
#[case("terminal-tabs")]
fn partial_tab_settings_preserve_defaults(#[case] section: &str) {
    let file = assert_fs::NamedTempFile::new("config.toml").unwrap();
    file.write_str(&format!("[chrome.{section}]\nclose-position = \"left\"\n"))
        .unwrap();
    let config = load_config_from_path(file.path()).unwrap();
    let defaults = BoottyConfig::default();
    assert_eq!(
        config.chrome.tabs.close_position,
        bootty_config::config::TabClosePosition::Left
    );
    assert_eq!(
        config.chrome.tabs.appearance,
        defaults.chrome.tabs.appearance
    );
    assert_eq!(
        config.chrome.tabs.close_button,
        defaults.chrome.tabs.close_button
    );
}

#[rstest::rstest]
#[case("chrome.panel-tab-style")]
#[case("chrome.panel-tabs")]
fn retired_placement_and_tab_preferences_do_not_offer_settings_controls(#[case] path: &str) {
    let schema = SettingsSchema::builtin();
    assert!(schema.allows_path(&path.split('.').collect::<Vec<_>>()));
    assert!(schema.get(path).is_none());
}

#[rstest::rstest]
#[case("manual", bootty_config::config::SidebarSortOrder::Manual)]
#[case(
    "recent-activity",
    bootty_config::config::SidebarSortOrder::RecentActivity
)]
fn session_sort_order_loads_through_the_scalar_setting(
    #[case] token: &str,
    #[case] expected: bootty_config::config::SidebarSortOrder,
) {
    let defaults = BoottyConfig::default();
    assert_eq!(
        defaults.sidebar.sort_order,
        bootty_config::config::SidebarSortOrder::Manual
    );
    let schema = SettingsSchema::builtin();
    let spec = schema
        .get("sidebar.sort-order")
        .expect("sort order setting");
    assert_eq!(spec.page, "sidebar");
    assert!(matches!(spec.kind, SettingKind::Choice { .. }));
    assert_eq!(
        spec.default_value(&defaults),
        Some(SettingValue::Token("manual".to_owned()))
    );
    let config =
        load_with(&["sidebar", "sort-order"], &format!("\"{token}\"")).expect("sort token");
    assert_eq!(config.sidebar.sort_order, expected);
    assert_eq!(
        spec.default_value(&config),
        Some(SettingValue::Token(token.to_owned()))
    );
    assert!(load_with(&["sidebar", "sort-order"], "\"focus-time\"").is_err());
}

/// Setting help renders as markdown, where a `<word>` outside a code span is an HTML tag and
/// disappears, so placeholders go in backticks.
#[test]
fn help_text_keeps_placeholders_in_code_spans() {
    for spec in SettingsSchema::builtin().specs() {
        let prose = spec.help.split('`').step_by(2).collect::<String>();
        assert!(
            !prose.contains('<'),
            "{}: put `<…>` placeholders in backticks so the help shows them",
            spec.id()
        );
    }
}
