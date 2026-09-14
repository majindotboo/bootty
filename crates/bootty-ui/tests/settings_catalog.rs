#![cfg(test)]

//! Native settings catalog characterization.

use bootty_ui::gpui as bootty_gpui;
use std::collections::HashSet;

use assert_fs::{TempDir, prelude::*};
use bootty_config::settings_schema::{SettingEditor, SettingKind, SettingSpec, SettingsSchema};
use bootty_ui::gpui::{ScalarValue, SettingsCategory, SettingsRow};
use bootty_ui::{
    UnsupportedModuleDiagnostic, advanced_configuration_rows, scan_unsupported_module_sources,
    setting_is_visible_in_native_settings, settings_catalog_pages, settings_category_for,
    settings_dependency_for, unsupported_module_rows,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn native_settings_catalog_keeps_the_zed_page_snapshot() {
    let snapshot = settings_catalog_pages()
        .iter()
        .map(|page| (page.category, page.id, page.label))
        .collect::<Vec<_>>();

    assert_eq!(
        snapshot,
        vec![
            (SettingsCategory::General, "general", "General"),
            (SettingsCategory::Appearance, "appearance", "Appearance"),
            (SettingsCategory::Keymap, "keymap", "Keymap"),
            (
                SettingsCategory::WindowAndLayout,
                "window-and-layout",
                "Window & Layout",
            ),
            (SettingsCategory::Panels, "panels", "Panels"),
            (SettingsCategory::Terminal, "terminal", "Terminal"),
            (SettingsCategory::Remotes, "remotes", "Remotes"),
            (SettingsCategory::Advanced, "advanced", "Advanced"),
        ]
    );
}

#[rstest]
fn unsupported_custom_sources_remain_visible_without_an_edit_action() {
    let rows = unsupported_module_rows(&[UnsupportedModuleDiagnostic {
        path: "/tmp/bootty/extensions/custom.luau".into(),
        detail: "native feature has no matching owner".to_owned(),
    }]);

    assert!(matches!(
        rows.as_slice(),
        [SettingsRow::Notice { text, destructive: false }]
            if text == "Unsupported custom module source preserved: /tmp/bootty/extensions/custom.luau (native feature has no matching owner)"
    ));
}

#[rstest]
fn unsupported_source_scan_is_read_only_and_recursive() {
    let directory = TempDir::new().expect("temporary config directory");
    directory
        .child("extensions/custom.luau")
        .write_str("return { source = 'must not be read' }")
        .expect("custom source");
    directory
        .child("extensions/nested/other.lua")
        .write_str("return {}")
        .expect("nested source");
    directory
        .child("extensions/ignored.txt")
        .write_str("not a module")
        .expect("non module");
    directory
        .child("status/legacy.lua")
        .write_str("return {}")
        .expect("legacy status source");

    let diagnostics = scan_unsupported_module_sources(directory.path()).expect("scan succeeds");
    assert_eq!(
        diagnostics
            .iter()
            .map(|diagnostic| diagnostic.path.strip_prefix(directory.path()).unwrap())
            .collect::<Vec<_>>(),
        vec![
            std::path::Path::new("extensions/custom.luau"),
            std::path::Path::new("extensions/nested/other.lua"),
            std::path::Path::new("status/legacy.lua"),
        ]
    );
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| diagnostic.detail == "native script execution is retired")
    );
}

#[rstest]
fn unsupported_source_scan_reports_the_depth_bound() {
    let directory = TempDir::new().expect("temporary config directory");
    let mut nested = directory.path().join("session");
    for _ in 0..17 {
        nested.push("nested");
    }
    std::fs::create_dir_all(&nested).expect("nested source directories");
    std::fs::write(nested.join("hidden.luau"), "return {}").expect("deep source");

    let diagnostics = scan_unsupported_module_sources(directory.path()).expect("scan succeeds");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.path == directory.path().join("session")
            && diagnostic.detail.contains("scan incomplete")
    }));
    assert!(
        diagnostics
            .iter()
            .all(|diagnostic| !diagnostic.path.ends_with("hidden.luau"))
    );
}

#[rstest]
#[case(None, "No write errors", "No settings write errors recorded.", false)]
#[case(
    Some("permission denied"),
    "Last write failed",
    "permission denied",
    true
)]
fn advanced_configuration_keeps_locations_and_write_status_visible(
    #[case] write_error: Option<&str>,
    #[case] summary: &str,
    #[case] detail: &str,
    #[case] detail_is_destructive: bool,
) {
    let rows =
        advanced_configuration_rows(std::path::Path::new("/tmp/bootty/config.toml"), write_error);

    let paths = rows
        .iter()
        .filter_map(|row| match row {
            SettingsRow::Value {
                id,
                label,
                value: ScalarValue::Text(path),
                control: bootty_gpui::SettingsControl::ReadOnly,
                enabled: true,
                ..
            } => Some((id.as_str(), label.as_str(), path.as_str())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![
            ("config.path", "Config file", "/tmp/bootty/config.toml"),
            ("config.directory", "Config directory", "/tmp/bootty"),
            (
                "config.themes-directory",
                "Themes directory",
                "/tmp/bootty/themes"
            ),
            (
                "config.extensions-directory",
                "Extensions directory",
                "/tmp/bootty/extensions"
            ),
        ]
    );
    assert!(rows.iter().any(|row| matches!(
        row,
        SettingsRow::Action { id, .. } if id == "config:reload"
    )));
    assert!(rows.iter().any(|row| matches!(
        row,
        SettingsRow::Notice { text, destructive: false } if text == summary
    )));
    assert!(rows.iter().any(|row| matches!(
        row,
        SettingsRow::Notice { text, destructive }
            if text == detail && *destructive == detail_is_destructive
    )));
}

#[rstest]
fn every_native_setting_has_one_catalog_destination() {
    let destinations = settings_catalog_pages()
        .iter()
        .map(|page| page.category)
        .collect::<HashSet<_>>();
    let mut ids = HashSet::new();

    for spec in SettingsSchema::builtin().specs() {
        assert!(
            ids.insert(spec.id()),
            "duplicate schema setting: {}",
            spec.id()
        );
        if setting_is_visible_in_native_settings(&spec.id()) {
            assert!(
                destinations.contains(&settings_category_for(&spec.id(), spec.page.as_ref())),
                "{} has no native settings destination",
                spec.id()
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NativeSettingsSurface {
    EditableRow,
    AggregateEditor(&'static str),
    Replacement(&'static str),
}

fn custom_setting_surface(spec: &SettingSpec, editor: SettingEditor) -> NativeSettingsSurface {
    match spec.id().as_str() {
        // These compatibility declarations are replaced by the explicit light/dark branch rows.
        "theme" | "colors.*" => NativeSettingsSurface::Replacement("appearance branches"),

        // Remotes are edited as one lifecycle-aware form rather than independent schema leaves.
        _ if editor == SettingEditor::Remotes => {
            NativeSettingsSurface::AggregateEditor(editor.name())
        }

        // Preserved extension settings have no native editor after script retirement.
        "extensions.*" => NativeSettingsSurface::Replacement("unsupported custom module"),

        "appearance.mode"
        | "appearance.light.theme"
        | "appearance.light.colors.*"
        | "appearance.dark.theme"
        | "appearance.dark.colors.*"
        | "cursor.style"
        | "font.family"
        | "font.ui-family"
        | "font.ui-use-terminal-family"
        | "font.features"
        | "font.cell-width"
        | "font.cell-height"
        | "chrome.top-bar"
        | "chrome.status-background"
        | "chrome.pane-divider-color"
        | "chrome.notched-fullscreen-black-chrome"
        | "chrome.pane-focus-border-color"
        | "chrome.top-segment"
        | "chrome.bottom-segment"
        | "sidebar.background"
        | "sidebar.foreground"
        | "sidebar.selected"
        | "sidebar.hover"
        | "sidebar.border"
        | "multiplexer.backend"
        | "input.modifier-remap"
        | "input.macos-option-as-alt"
        | "input.hide-mouse-pointer-while-typing"
        | "input.copy-on-select"
        | "input.preset"
        | "input.prefix"
        | "session.env"
        | "session.max-scrollback"
        | "window.fullscreen-top-offset" => NativeSettingsSurface::EditableRow,

        // Legacy module composition is accepted from existing config, while the native settings
        // surface is driven by the same panel registry as Dock menus and commands.
        "sidebar.session-modules" | "sidebar.modules" => {
            NativeSettingsSurface::Replacement("panel registry")
        }

        // The dedicated keymap editor is the sole writer for keybinding arrays.
        "input.keybind"
        | "input.sidebar-keybind"
        | "input.backend-keybind.herdr"
        | "input.backend-keybind.native"
        | "input.backend-keybind.rmux"
        | "input.backend-keybind.tmux" => NativeSettingsSurface::Replacement("keymap editor"),

        id => panic!(
            "custom setting {id} owned by {} has no explicit GPUI surface contract",
            editor.name()
        ),
    }
}

fn native_setting_surface(spec: &SettingSpec) -> NativeSettingsSurface {
    match &spec.kind {
        SettingKind::Custom(editor) => custom_setting_surface(spec, *editor),
        SettingKind::Bool
        | SettingKind::Text { .. }
        | SettingKind::Number { .. }
        | SettingKind::Choice { .. }
        | SettingKind::FontStyle => NativeSettingsSurface::EditableRow,
    }
}

#[rstest]
fn every_builtin_schema_setting_has_an_explicit_gpui_editing_surface() {
    let schema = SettingsSchema::builtin();
    let destinations = settings_catalog_pages()
        .iter()
        .map(|page| page.category)
        .collect::<HashSet<_>>();

    for spec in schema.specs() {
        let id = spec.id();
        let surface = native_setting_surface(spec);
        if setting_is_visible_in_native_settings(&id) {
            assert!(
                destinations.contains(&settings_category_for(&id, spec.page.as_ref())),
                "visible setting {id} has no GPUI destination"
            );
        } else {
            assert!(
                matches!(surface, NativeSettingsSurface::Replacement(_)),
                "hidden setting {id} must name its replacement surface, got {surface:?}"
            );
        }
    }
}

#[rstest]
fn aggregate_and_replacement_surfaces_are_deliberate_and_named() {
    let aggregate = SettingsSchema::builtin()
        .specs()
        .iter()
        .filter_map(|spec| match native_setting_surface(spec) {
            NativeSettingsSurface::AggregateEditor(owner) => Some((spec.id(), owner)),
            NativeSettingsSurface::EditableRow | NativeSettingsSurface::Replacement(_) => None,
        })
        .collect::<Vec<_>>();
    let replacements = SettingsSchema::builtin()
        .specs()
        .iter()
        .filter_map(|spec| match native_setting_surface(spec) {
            NativeSettingsSurface::Replacement(owner) => Some((spec.id(), owner)),
            NativeSettingsSurface::EditableRow | NativeSettingsSurface::AggregateEditor(_) => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(
        aggregate,
        vec![
            ("multiplexer.remote.distribution".to_owned(), "remotes"),
            ("multiplexer.remote.host".to_owned(), "remotes"),
            ("multiplexer.remote.user".to_owned(), "remotes"),
            ("multiplexer.remote.port".to_owned(), "remotes"),
            ("multiplexer.remote.program".to_owned(), "remotes"),
            ("multiplexer.remote.args".to_owned(), "remotes"),
            ("ssh-profiles.*.name".to_owned(), "remotes"),
            ("ssh-profiles.*.host".to_owned(), "remotes"),
            ("ssh-profiles.*.user".to_owned(), "remotes"),
            ("ssh-profiles.*.port".to_owned(), "remotes"),
            ("ssh-profiles.*.authentication".to_owned(), "remotes"),
            ("ssh-profiles.*.host-key-policy".to_owned(), "remotes"),
            ("ssh-profiles.*.identity-file".to_owned(), "remotes"),
            ("ssh-profiles.*.proxy-jump".to_owned(), "remotes"),
            ("ssh-profiles.*.program".to_owned(), "remotes"),
            ("ssh-profiles.*.args".to_owned(), "remotes"),
        ]
    );
    assert_eq!(
        replacements,
        vec![
            ("theme".to_owned(), "appearance branches"),
            ("colors.*".to_owned(), "appearance branches"),
            ("sidebar.session-modules".to_owned(), "panel registry"),
            ("sidebar.modules".to_owned(), "panel registry"),
            ("input.keybind".to_owned(), "keymap editor"),
            ("input.sidebar-keybind".to_owned(), "keymap editor"),
            ("input.backend-keybind.herdr".to_owned(), "keymap editor"),
            ("input.backend-keybind.native".to_owned(), "keymap editor"),
            ("input.backend-keybind.rmux".to_owned(), "keymap editor"),
            ("input.backend-keybind.tmux".to_owned(), "keymap editor"),
            ("extensions.*".to_owned(), "unsupported custom module"),
        ]
    );
}

#[rstest]
#[case("input.keybind")]
#[case("input.sidebar-keybind")]
#[case("input.backend-keybind.herdr")]
#[case("input.backend-keybind.native")]
#[case("input.backend-keybind.rmux")]
#[case("input.backend-keybind.tmux")]
fn legacy_toml_keybinding_inputs_do_not_compete_with_the_keymap_editor(#[case] id: &str) {
    assert!(!setting_is_visible_in_native_settings(id));
}

#[rstest]
fn modifier_remapping_stays_on_the_keymap_page() {
    assert!(setting_is_visible_in_native_settings(
        "input.modifier-remap"
    ));
    assert_eq!(
        settings_category_for("input.modifier-remap", "keys"),
        SettingsCategory::Keymap
    );
}

#[rstest]
#[case("input.preset")]
#[case("input.prefix")]
fn base_keymap_controls_stay_visible_on_the_keymap_page(#[case] id: &str) {
    assert!(setting_is_visible_in_native_settings(id));
    assert_eq!(settings_category_for(id, "keys"), SettingsCategory::Keymap);
}

#[rstest]
fn appearance_mode_owns_the_branch_theme_children() {
    let dependency = settings_dependency_for("appearance.mode")
        .expect("appearance mode must retain its branch theme children");

    assert_eq!(dependency.parent, "appearance.mode");
    assert_eq!(
        dependency.children,
        &["appearance.light.theme", "appearance.dark.theme"]
    );
    for child in dependency.children {
        assert!(setting_is_visible_in_native_settings(child));
        assert_eq!(
            settings_category_for(child, "colors"),
            SettingsCategory::Appearance
        );
    }
    assert_eq!(settings_dependency_for("appearance.light.theme"), None);
    assert_eq!(settings_dependency_for("appearance.dark.theme"), None);
}

#[rstest]
#[case(
    "system",
    &["appearance.light.theme", "appearance.dark.theme"]
)]
#[case("light", &["appearance.light.theme", "appearance.dark.theme"])]
#[case("dark", &["appearance.light.theme", "appearance.dark.theme"])]
#[case("unknown", &[])]
fn appearance_mode_keeps_both_theme_branches_editable(
    #[case] mode: &str,
    #[case] expected: &[&str],
) {
    let dependency = settings_dependency_for("appearance.mode")
        .expect("appearance mode must retain its branch theme children");

    assert_eq!(
        dependency.active_children(&ScalarValue::Token(mode.to_owned())),
        expected
    );
}

#[rstest]
#[case("system", "appearance.light.colors.background", true)]
#[case("system", "appearance.dark.colors.palette", true)]
#[case("light", "appearance.light.colors.palette-generate", true)]
#[case("light", "appearance.dark.colors.foreground", true)]
#[case("dark", "appearance.dark.colors.selection-background", true)]
#[case("dark", "appearance.light.colors.cursor", true)]
#[case("unknown", "appearance.light.colors.background", false)]
fn appearance_mode_keeps_both_color_branches_editable(
    #[case] mode: &str,
    #[case] id: &str,
    #[case] expected: bool,
) {
    let dependency = settings_dependency_for("appearance.mode")
        .expect("appearance mode must retain its branch color children");

    assert_eq!(
        dependency.is_child_active(id, &ScalarValue::Token(mode.to_owned())),
        expected
    );
}

#[rstest]
#[case(
    "window.fullscreen-enabled",
    SettingsCategory::WindowAndLayout,
    ScalarValue::Bool(true),
    &[
        "window.fullscreen",
        "window.fullscreen-tabs-in-notch",
        "window.fullscreen-top-offset",
        "chrome.notched-fullscreen-black-chrome",
    ],
    ScalarValue::Bool(false),
    &[
        "window.fullscreen",
        "window.fullscreen-tabs-in-notch",
        "window.fullscreen-top-offset",
        "chrome.notched-fullscreen-black-chrome",
    ],
)]
#[case(
    "font.ui-use-terminal-family",
    SettingsCategory::Appearance,
    ScalarValue::Bool(false),
    &["font.ui-family"],
    ScalarValue::Bool(true),
    &[],
)]
fn native_catalog_projects_each_boolean_dependent_group_once(
    #[case] parent: &str,
    #[case] category: SettingsCategory,
    #[case] active_value: ScalarValue,
    #[case] expected_children: &[&str],
    #[case] inactive_value: ScalarValue,
    #[case] expected_inactive_children: &[&str],
) {
    let dependency = settings_dependency_for(parent).expect("dependency is registered");

    assert_eq!(dependency.parent, parent);
    assert_eq!(dependency.children, expected_children);
    assert_eq!(dependency.active_children(&active_value), expected_children);
    assert_eq!(
        dependency.active_children(&inactive_value),
        expected_inactive_children
    );

    for child in expected_children {
        let legacy_page = match *child {
            "font.ui-family" => "text",
            _ => "window",
        };
        assert!(
            setting_is_visible_in_native_settings(child),
            "{child} must remain a real native settings row"
        );
        assert_eq!(
            settings_category_for(child, legacy_page),
            category,
            "{child} must stay on its parent's page so the projection can make one dependent item"
        );
        assert!(
            settings_dependency_for(child).is_none(),
            "{child} must not start a second dependent group"
        );
        assert!(dependency.is_child_active(child, &active_value));
        assert_eq!(
            dependency.is_child_active(child, &inactive_value),
            expected_inactive_children.contains(child)
        );
    }
}

#[rstest]
#[case("font.ui-size")]
#[case("font.family")]
#[case("font.cell-width")]
#[case("font.cell-height")]
fn ui_terminal_font_selection_does_not_hide_unrelated_font_metrics(#[case] id: &str) {
    let dependency = settings_dependency_for("font.ui-use-terminal-family")
        .expect("UI terminal-font selection has a dependent UI family row");

    assert!(!dependency.children.contains(&id));
    assert!(!dependency.is_child_active(id, &ScalarValue::Bool(true)));
    assert!(!dependency.is_child_active(id, &ScalarValue::Bool(false)));
}

#[rstest]
#[case("restore_on_startup", "general", SettingsCategory::General)]
#[case("cli_default_open_behavior", "general", SettingsCategory::General)]
#[case("default_open_behavior", "general", SettingsCategory::General)]
#[case("when_closing_with_no_tabs", "general", SettingsCategory::General)]
#[case("on_last_window_closed", "general", SettingsCategory::General)]
#[case("multiplexer.backend", "general", SettingsCategory::General)]
#[case("appearance.mode", "colors", SettingsCategory::Appearance)]
#[case("font.ui-family", "text", SettingsCategory::Appearance)]
#[case("font.family", "text", SettingsCategory::Appearance)]
#[case("cursor.style", "appearance", SettingsCategory::Appearance)]
#[case("sidebar.background", "colors", SettingsCategory::Appearance)]
#[case("chrome.pane-divider-color", "colors", SettingsCategory::Appearance)]
#[case(
    "input.hide-mouse-pointer-while-typing",
    "appearance",
    SettingsCategory::Appearance
)]
#[case("input.preset", "keys", SettingsCategory::Keymap)]
#[case("input.prefix", "keys", SettingsCategory::Keymap)]
#[case("input.backend-keybind.tmux", "keys", SettingsCategory::Keymap)]
#[case("input.modifier-remap", "keys", SettingsCategory::Keymap)]
#[case(
    "window.fullscreen-enabled",
    "window",
    SettingsCategory::WindowAndLayout
)]
#[case(
    "chrome.pane-divider-width",
    "window",
    SettingsCategory::WindowAndLayout
)]
#[case("session.max-scrollback", "shell", SettingsCategory::Terminal)]
#[case("input.copy-on-select", "keys", SettingsCategory::Terminal)]
#[case("input.macos-option-as-alt", "keys", SettingsCategory::Terminal)]
#[case("chrome.top-segment", "status", SettingsCategory::Panels)]
#[case("sidebar.session-modules", "sidebar", SettingsCategory::Panels)]
#[case("multiplexer.remote.host", "remotes", SettingsCategory::Remotes)]
#[case(
    "diagnostics.stability-trace",
    "diagnostics",
    SettingsCategory::Advanced
)]
#[case("extensions.example.setting", "extensions", SettingsCategory::Advanced)]
fn catalog_routes_each_zed_taxonomy_boundary(
    #[case] id: &str,
    #[case] legacy_page: &str,
    #[case] expected: SettingsCategory,
) {
    assert_eq!(settings_category_for(id, legacy_page), expected);
}
