use super::*;
use crate::color::Color;
use bootty_winit::input_binding::{
    BindingAction, BindingElement, BindingTrigger, parse_binding_elements,
};
use bootty_winit::input_binding_set::BindingSet;
use indoc::indoc;
use proptest::prelude::*;
use rstest::rstest;
use std::str::FromStr;
use std::{collections::BTreeMap, fs};

struct ConfigSandbox {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl ConfigSandbox {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        Self { _dir: dir, path }
    }

    fn with_config(source: &str) -> Self {
        let sandbox = Self::new();
        sandbox.write("config.toml", source);
        sandbox
    }

    fn write(&self, relative_path: &str, source: &str) {
        let path = self._dir.path().join(relative_path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, source).unwrap();
    }

    fn load(&self) -> Result<BoottyConfig, ConfigLoadError> {
        load_config_from_path(&self.path)
    }
}

fn load_config_source(source: &str) -> BoottyConfig {
    ConfigSandbox::with_config(source).load().unwrap()
}

fn integer_table(values: &BTreeMap<String, i64>) -> Table {
    let mut table = Table::new();
    for (key, value) in values {
        table.insert(key, toml_edit::value(*value));
    }
    table
}

proptest! {
    #[test]
    fn merge_toml_tables_preserves_base_keys_not_present_in_overlay(
        base in proptest::collection::btree_map("[a-z][a-z0-9]{0,5}", any::<i64>(), 0..12),
        overlay in proptest::collection::btree_map("[m-z][a-z0-9]{0,5}", any::<i64>(), 0..12),
    ) {
        let mut target = integer_table(&base);
        let overlay_table = integer_table(&overlay);

        merge_toml_tables(&mut target, overlay_table);

        for (key, value) in base {
            if !overlay.contains_key(&key) {
                prop_assert_eq!(target[&key].as_integer(), Some(value));
            }
        }
    }

    #[test]
    fn merge_toml_tables_uses_overlay_values_for_matching_scalar_keys(
        key in "[a-z][a-z0-9]{0,5}",
        base_value in any::<i64>(),
        overlay_value in any::<i64>(),
    ) {
        let mut target = integer_table(&BTreeMap::from([(key.clone(), base_value)]));
        let overlay = integer_table(&BTreeMap::from([(key.clone(), overlay_value)]));

        merge_toml_tables(&mut target, overlay);

        prop_assert_eq!(target[&key].as_integer(), Some(overlay_value));
    }

}

#[rstest]
#[case::self_cycle(&[("a.toml", "a.toml")], "a.toml")]
#[case::two_file_cycle(&[("a.toml", "b.toml"), ("b.toml", "a.toml")], "a.toml")]
#[case::cycle_after_acyclic_entry(
    &[("entry.toml", "a.toml"), ("a.toml", "b.toml"), ("b.toml", "a.toml")],
    "entry.toml"
)]
fn include_cycles_are_rejected(#[case] edges: &[(&str, &str)], #[case] entry: &str) {
    let dir = tempfile::tempdir().unwrap();
    for (source, target) in edges {
        fs::write(
            dir.path().join(source),
            format!(
                indoc! {r#"
                    include = ["{target}"]
                "#},
                target = target
            ),
        )
        .unwrap();
    }

    assert!(load_config_from_path(dir.path().join(entry)).is_err());
}

#[test]
fn merge_toml_tables_recurses_into_nested_tables() {
    let mut target = indoc! {r#"
        [window]
        title = "base"
        width = 1000
    "#}
    .parse::<DocumentMut>()
    .unwrap();
    let overlay = indoc! {r#"
        [window]
        title = "included"
    "#}
    .parse::<DocumentMut>()
    .unwrap();

    merge_toml_tables(target.as_table_mut(), overlay.into_table());

    assert_eq!(target["window"]["title"].as_str(), Some("included"));
    assert_eq!(target["window"]["width"].as_integer(), Some(1000));
}

#[rstest]
#[case(Some("/tmp/xdg"), Some("/tmp/home"), "/tmp/xdg/bootty/config.toml")]
#[case(None, Some("/tmp/home"), "/tmp/home/.config/bootty/config.toml")]
fn config_path_prefers_xdg_then_home(
    #[case] xdg: Option<&str>,
    #[case] home: Option<&str>,
    #[case] expected: &str,
) {
    assert_eq!(config_path_from_env(xdg, home), PathBuf::from(expected));
}

#[test]
fn empty_session_working_directory_resolves_to_home() {
    let expected_home = default_working_directory().expect("home directory should be discoverable");

    assert_eq!(
        SessionConfig::default().launch_config().working_directory,
        Some(expected_home)
    );
}

#[test]
fn configured_session_working_directory_overrides_home_default() {
    let config = SessionConfig {
        working_directory: Some(PathBuf::from("tmp/bootty-project")),
        ..SessionConfig::default()
    };

    assert_eq!(
        config.launch_config().working_directory,
        Some(PathBuf::from("tmp/bootty-project"))
    );
}

#[test]
fn missing_config_file_loads_with_selected_path() {
    let sandbox = ConfigSandbox::new();

    let config = sandbox.load().unwrap();

    assert_eq!(config.config_path, sandbox.path);
}

#[test]
fn defaults_include_session_status_segment_before_windows() {
    let config = load_config_source("");
    let modules = config
        .chrome
        .status_segments
        .iter()
        .map(|segment| segment.module.as_str())
        .collect::<Vec<_>>();
    let session = modules
        .iter()
        .position(|module| *module == "session")
        .expect("session status module is enabled by default");
    let windows = modules
        .iter()
        .position(|module| *module == "windows")
        .expect("windows status module is enabled by default");

    assert!(session < windows, "session should appear before windows");
}

#[test]
fn included_file_overrides_containing_file_without_dropping_base_keys() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        include = ["local.toml"]

        [window]
        title = "base"
        width = 1000
    "#});
    sandbox.write(
        "local.toml",
        indoc! {r#"
            [window]
            title = "local"
            height = 640
        "#},
    );

    let config = sandbox.load().unwrap();

    assert_eq!(config.window.title, "local");
    assert_eq!(config.window.width, 1000.0);
    assert_eq!(config.window.height, 640.0);
}

#[test]
fn config_file_snapshot_changes_when_included_file_changes() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        include = ["local.toml"]
    "#});
    sandbox.write(
        "local.toml",
        indoc! {r#"
            [window]
            title = "before"
        "#},
    );

    let before = config_file_snapshot(&sandbox.path).unwrap();
    sandbox.write(
        "local.toml",
        indoc! {r#"
            [window]
            title = "after"
            width = 900
        "#},
    );
    let after = before.refresh_known_paths();

    assert_ne!(before, after);
}

#[rstest]
#[case::builtin_theme(
    indoc! {r#"
        theme = "Catppuccin Mocha"
    "#},
    Some("Catppuccin Mocha"),
    Some(Color::from_hex("#1e1e2e").unwrap()),
    Some(Color::from_hex("#cdd6f4").unwrap()),
    16
)]
#[case::explicit_color_override(
    indoc! {r##"
        theme = "Catppuccin Mocha"

        [colors]
        background = "#101112"
        palette = ["#000000", "#111111"]
    "##},
    Some("Catppuccin Mocha"),
    Some(Color::from_hex("#101112").unwrap()),
    Some(Color::from_hex("#cdd6f4").unwrap()),
    2
)]
fn config_resolves_theme_and_color_overrides(
    #[case] source: &str,
    #[case] theme: Option<&str>,
    #[case] background: Option<Color>,
    #[case] foreground: Option<Color>,
    #[case] palette_len: usize,
) {
    let config = load_config_source(source);

    assert_eq!(config.theme.as_deref(), theme);
    assert_eq!(config.colors.background, background);
    assert_eq!(config.colors.foreground, foreground);
    assert_eq!(config.colors.palette.len(), palette_len);
}

#[test]
fn appearance_branches_resolve_separate_themes_and_overrides() {
    let config = load_config_source(indoc! {r##"
        [appearance]
        mode = "light"

        [appearance.light]
        theme = "Atom One Light"

        [appearance.light.colors]
        background = "#fefefe"

        [appearance.dark]
        theme = "Dracula"
    "##});

    assert_eq!(config.appearance.mode, AppearanceMode::Light);
    assert_eq!(
        config.appearance.light.theme.as_deref(),
        Some("Atom One Light")
    );
    assert_eq!(
        config.appearance.light.colors.background,
        Some(Color::from_hex("#fefefe").unwrap())
    );
    assert_eq!(config.appearance.dark.theme.as_deref(), Some("Dracula"));
    assert_eq!(
        config.appearance.dark.colors.background,
        Some(Color::from_hex("#282a36").unwrap())
    );
}

#[test]
fn legacy_theme_and_colors_seed_appearance_branches() {
    let config = load_config_source(indoc! {r##"
        theme = "Catppuccin Mocha"

        [colors]
        background = "#101112"
    "##});

    for branch in [&config.appearance.light, &config.appearance.dark] {
        assert_eq!(branch.theme.as_deref(), Some("Catppuccin Mocha"));
        assert_eq!(
            branch.colors.background,
            Some(Color::from_hex("#101112").unwrap())
        );
    }
    assert_eq!(config.theme.as_deref(), Some("Catppuccin Mocha"));
    assert_eq!(config.colors, config.appearance.dark.colors);
}

#[test]
fn config_resolves_sidebar_and_status_chrome_colors() {
    let config = load_config_source(indoc! {r##"
        [chrome]
        status-background = "#090909"
        notched-fullscreen-black-chrome = false

        [sidebar]
        position = "right"
        background = "#11131a"
        foreground = "#cdd6f4"
        selected = "#2a2f3d"
        hover = "#1e222c"
        border = "#313244"
    "##});

    assert_eq!(config.sidebar.position, SidebarPosition::Right);
    assert_eq!(
        config.chrome.status_background,
        Some(Color::from_hex("#090909").unwrap())
    );
    assert!(!config.chrome.notched_fullscreen_black_chrome);
    assert_eq!(
        config.sidebar.background,
        Some(Color::from_hex("#11131a").unwrap())
    );
    assert_eq!(
        config.sidebar.foreground,
        Some(Color::from_hex("#cdd6f4").unwrap())
    );
    assert_eq!(
        config.sidebar.selected,
        Some(Color::from_hex("#2a2f3d").unwrap())
    );
    assert_eq!(
        config.sidebar.hover,
        Some(Color::from_hex("#1e222c").unwrap())
    );
    assert_eq!(
        config.sidebar.border,
        Some(Color::from_hex("#313244").unwrap())
    );
}

#[test]
fn config_defaults_sidebar_to_left_without_overrides() {
    let config = load_config_source("");

    assert_eq!(config.sidebar.position, SidebarPosition::Left);
    assert_eq!(config.sidebar.background, None);
    assert_eq!(config.chrome.status_background, None);
    assert!(config.chrome.notched_fullscreen_black_chrome);
}

#[test]
fn legacy_sidebar_fullscreen_colors_are_accepted_but_ignored() {
    let config = load_config_source(indoc! {r##"
        [sidebar]
        fullscreen-background = "#000000"
        fullscreen-hover = "#111111"
    "##});

    assert_eq!(config.sidebar.background, None);
    assert_eq!(config.sidebar.hover, None);
}

#[test]
fn config_overrides_fullscreen_top_offset() {
    let config = load_config_source(indoc! {r#"
        [window]
        fullscreen-top-offset = 40
    "#});

    assert_eq!(config.window.fullscreen_top_offset, Some(40.0));
    // Absent key keeps auto-detection (None).
    assert_eq!(load_config_source("").window.fullscreen_top_offset, None);
}

#[test]
fn config_toggles_fullscreen_tabs_in_notch() {
    let config = load_config_source(indoc! {r#"
        [window]
        fullscreen-tabs-in-notch = false
    "#});

    assert!(!config.window.fullscreen_tabs_in_notch);
    // Defaults to on so the notch band is used out of the box.
    assert!(load_config_source("").window.fullscreen_tabs_in_notch);
}

#[test]
fn config_accepts_ghostty_palette_generation_settings() {
    let config = load_config_source(indoc! {r##"
        [colors]
        background = "#ffffff"
        foreground = "#000000"
        palette = ["#000000", "#111111"]
        palette-generate = true
        palette-harmonious = true
    "##});

    assert!(config.colors.palette_generate);
    assert!(config.colors.palette_harmonious);

    let terminal_colors = config.colors.terminal_color_config();
    assert!(terminal_colors.palette_generate);
    assert!(terminal_colors.palette_harmonious);
}

#[test]
fn config_maps_font_features_to_terminal_text_config() {
    let config = load_config_source(indoc! {r#"
        font-feature = ["cv01", "ss05"]

        [font]
        features = ["cv33", "-calt"]
    "#});

    let features = config.font.terminal_text_config().font_features;

    assert!(features.contains(&FontFeature::new(*b"liga", 1)));
    assert!(features.contains(&FontFeature::new(*b"cv01", 1)));
    assert!(features.contains(&FontFeature::new(*b"ss05", 1)));
    assert!(features.contains(&FontFeature::new(*b"cv33", 1)));
    assert!(features.contains(&FontFeature::new(*b"calt", 0)));
}

#[test]
fn config_maps_font_fit_cell_height_to_terminal_text_config() {
    assert!(load_config_source("").font.fit_cell_height);

    let config = load_config_source(indoc! {r#"
        [font]
        fit-cell-height = false
    "#});

    assert!(!config.font.fit_cell_height);
    assert!(!config.font.terminal_text_config().fit_cell_height);
}

#[test]
fn config_uses_auto_font_cell_metrics_until_width_or_height_is_configured() {
    let default = load_config_source("");
    assert_eq!(default.font.cell_width, None);
    assert_eq!(default.font.cell_height, None);

    let config = load_config_source(indoc! {r#"
        [font]
        cell-width = 11
        cell-height = 24
    "#});

    assert_eq!(config.font.cell_width, Some(11.0));
    assert_eq!(config.font.cell_height, Some(24.0));
}

#[test]
fn config_rejects_invalid_font_features() {
    let error = ConfigSandbox::with_config(indoc! {r#"
        [font]
        features = ["toolong"]
    "#})
    .load()
    .unwrap_err();

    assert!(error.to_string().contains("invalid font feature"));
}

#[test]
fn config_accepts_xterm_dynamic_color_slots() {
    let config = load_config_source(indoc! {r##"
        [colors]
        pointer-foreground = "#010203"
        pointer-background = "#040506"
        tektronix-foreground = "#070809"
        tektronix-background = "#0a0b0c"
        highlight-background = "#0d0e0f"
        tektronix-cursor = "#101112"
        highlight-foreground = "#131415"
    "##});

    assert_eq!(
        config.colors.pointer_foreground,
        Some(Color::from_hex("#010203").unwrap())
    );
    assert_eq!(
        config.colors.highlight_foreground,
        Some(Color::from_hex("#131415").unwrap())
    );

    let terminal_colors = config.colors.terminal_color_config();
    assert_eq!(
        terminal_colors.pointer_background,
        Some(Color::from_hex("#040506").unwrap().into())
    );
    assert_eq!(
        terminal_colors.tektronix_cursor,
        Some(Color::from_hex("#101112").unwrap().into())
    );
}

#[test]
fn user_theme_shadows_builtin_theme_name() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        theme = "Catppuccin Mocha"
    "#});
    sandbox.write(
        "themes/Catppuccin Mocha.toml",
        indoc! {r##"
            [metadata]
            name = "Catppuccin Mocha"
            source = "test sandbox"
            license = "test"

            [colors]
            background = "#000102"
            foreground = "#030405"
        "##},
    );

    let config = sandbox.load().unwrap();

    assert_eq!(
        config.colors.background,
        Some(Color::from_hex("#000102").unwrap())
    );
    assert_eq!(
        config.colors.foreground,
        Some(Color::from_hex("#030405").unwrap())
    );
    assert!(config.colors.palette.is_empty());
}

#[test]
fn missing_theme_reports_user_and_builtin_locations() {
    let error = ConfigSandbox::with_config(indoc! {r#"
        theme = "No Such Theme"
    "#})
    .load()
    .unwrap_err();

    assert!(error.to_string().contains("No Such Theme"));
    assert!(error.to_string().contains("themes"));
    assert!(error.to_string().contains("built-in catalog"));
}

#[rstest]
#[case("?missing.toml", true)]
#[case("missing.toml", false)]
fn missing_include_behavior_depends_on_optional_marker(
    #[case] include: &str,
    #[case] should_load: bool,
) {
    let sandbox = ConfigSandbox::with_config(&format!(
        indoc! {r#"
            include = ["{include}"]

            [window]
            title = "ok"
        "#},
        include = include
    ));

    match sandbox.load() {
        Ok(config) if should_load => assert_eq!(config.window.title, "ok"),
        Err(_) if !should_load => {}
        result => panic!("unexpected missing include result: {result:?}"),
    }
}

#[test]
fn reload_keeps_last_good_config_when_new_config_is_invalid() {
    let sandbox = ConfigSandbox::new();
    let good_path = sandbox._dir.path().join("good.toml");
    let bad_path = sandbox._dir.path().join("bad.toml");
    sandbox.write(
        "good.toml",
        indoc! {r#"
            [window]
            title = "good"
        "#},
    );
    fs::write(
        &bad_path,
        indoc! {r#"
            [window
            title = "bad"
        "#},
    )
    .unwrap();

    let mut state = ConfigState::new(load_config_from_path(&good_path).unwrap());

    let error = state.reload_from_path(&bad_path).unwrap_err();

    assert!(error.to_string().contains("bad.toml"));
    assert_eq!(state.current().window.title, "good");
    assert!(state.last_error().unwrap().contains("bad.toml"));
}

#[test]
fn config_document_preserves_comments_and_order_for_writeback() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        # user comment
        include = ["?local.toml"]

        [window]
        # title comment
        title = "Bootty"
        width = 1220
    "#});
    let source = fs::read_to_string(&sandbox.path).unwrap();

    let document = load_config_document(&sandbox.path).unwrap().unwrap();

    assert_eq!(document.path(), sandbox.path);
    assert_eq!(document.to_toml_string(), source);
}

#[test]
fn config_document_writeback_preserves_unrelated_comments_and_order() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        # user comment
        include = ["?local.toml"]

        [font]
        # size comment
        size = 13

        [chrome]
        sidebar = true
    "#});
    let mut document = load_config_document(&sandbox.path).unwrap().unwrap();

    document
        .set_item(&["font", "size"], toml_edit::value(15.0))
        .unwrap();
    document.write_to_disk().unwrap();

    let written = fs::read_to_string(&sandbox.path).unwrap();
    assert!(written.contains("# user comment"));
    assert!(written.contains("# size comment"));
    assert!(written.contains("[chrome]\nsidebar = true"));
    assert!(written.find("include").unwrap() < written.find("[font]").unwrap());
    assert!(written.contains("size = 15.0"));
}

#[test]
fn font_size_preference_writeback_round_trips_through_config_loader() {
    let sandbox = ConfigSandbox::with_config(indoc! {r#"
        # top comment
        [window]
        title = "Keep Me"
    "#});

    write_font_size_preference(&sandbox.path, 16.0).unwrap();

    let written = fs::read_to_string(&sandbox.path).unwrap();
    assert!(written.contains("# top comment"));
    assert!(written.find("[window]").unwrap() < written.find("[font]").unwrap());
    assert_eq!(
        load_config_from_path(&sandbox.path).unwrap().font.size,
        16.0
    );
}

#[test]
fn documented_sample_config_loads() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/sample-config.toml");

    let config = load_config_from_path(&path).unwrap();

    assert_eq!(config.theme.as_deref(), Some("Catppuccin Mocha"));
}

#[test]
fn input_hide_mouse_pointer_while_typing_defaults_on_and_can_be_disabled() {
    assert!(
        BoottyConfig::default()
            .input
            .hide_mouse_pointer_while_typing
    );

    let config = load_config_source(indoc! {r#"
        [input]
        hide-mouse-pointer-while-typing = false
    "#});

    assert!(!config.input.hide_mouse_pointer_while_typing);
}

#[test]
fn config_maps_macos_option_as_alt_to_terminal_session_config() {
    let config = load_config_source(indoc! {r#"
        [input]
        macos-option-as-alt = "right"
    "#});

    assert_eq!(
        config.input.macos_option_as_alt,
        MacosOptionAsAltConfig::Right
    );
    assert_eq!(
        config.terminal_session_config().macos_option_as_alt,
        MacosOptionAsAlt::Right
    );
    assert_eq!(
        BoottyConfig::default()
            .terminal_session_config()
            .macos_option_as_alt,
        MacosOptionAsAlt::Both
    );
}

#[test]
fn config_maps_session_scrollback_to_terminal_session_config() {
    let config = load_config_source(indoc! {r#"
        [session]
        max-scrollback = 0
    "#});

    assert_eq!(config.session.max_scrollback, 0);
    assert_eq!(config.terminal_session_config().max_scrollback, 0);
    assert_eq!(
        BoottyConfig::default()
            .terminal_session_config()
            .max_scrollback,
        NATIVE_MAX_SCROLLBACK
    );
}

#[test]
fn config_maps_cursor_defaults_to_terminal_session_config() {
    let config = load_config_source(indoc! {r#"
        [cursor]
        style = "underline"
        blink = true
    "#});

    assert_eq!(config.cursor.style, Some(CursorStyleConfig::Underline));
    assert_eq!(config.cursor.blink, Some(true));
    assert_eq!(
        config.terminal_session_config().cursor,
        bootty_terminal::terminal_engine::TerminalCursorConfig {
            style: Some(bootty_terminal::terminal_engine::TerminalCursorStyle::Underline),
            blink: Some(true),
        }
    );
    assert_eq!(
        BoottyConfig::default().terminal_session_config().cursor,
        bootty_terminal::terminal_engine::TerminalCursorConfig::default()
    );
}

#[test]
fn config_maps_glyph_protocol_policy_to_terminal_features() {
    let config = load_config_source(indoc! {r#"
        [session]
        glyph-protocol = false
    "#});

    assert!(!config.session.glyph_protocol);
    assert!(!config.terminal_session_config().features.glyph_protocol);
    assert!(
        BoottyConfig::default()
            .terminal_session_config()
            .features
            .glyph_protocol
    );
}

#[test]
fn obsolete_chrome_window_tabs_key_is_ignored() {
    load_config_source(indoc! {r#"
        [chrome]
        window-tabs = true
    "#});
}
#[test]
fn keybind_clear_directive_replaces_existing_bindings() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        keybind = ["clear", "cmd+b=esc:090;8~"]
    "#});

    assert_eq!(config.input.keybind, vec!["cmd+b=esc:090;8~"]);
}

#[test]
fn keybind_entries_without_clear_layer_on_defaults() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        keybind = ["cmd+b=esc:090;8~"]
    "#});

    assert!(
        config.input.keybind.iter().any(|k| k == "cmd+b=esc:090;8~"),
        "user binding is kept"
    );
    assert!(
        config
            .input
            .keybind
            .iter()
            .any(|k| k == "shift+Enter=text:\\n"),
        "defaults the user did not list are retained"
    );
}

#[test]
fn sidebar_keybind_clear_directive_replaces_existing_bindings() {
    let config = load_config_source(indoc! {r#"
        version = 1

        [input]
        sidebar-keybind = ["clear", "space=activate_session"]
    "#});

    assert_eq!(config.input.sidebar_keybind, vec!["space=activate_session"]);
}

#[test]
fn config_accepts_native_multiplexer_backend() {
    let config = load_config_source(indoc! {r#"
        [multiplexer]
        backend = "native"
    "#});

    assert_eq!(config.multiplexer.backend, MultiplexerBackendConfig::Native);
}

// The platform default tables are cfg-selected, so these tests address both tables directly to
// validate the Linux/Windows table from any build host.
fn binding_triggers(entries: &[&str]) -> Vec<BindingTrigger> {
    entries
        .iter()
        .map(|entry| {
            parse_binding_elements(entry)
                .unwrap_or_else(|error| {
                    panic!("default keybind {entry:?} failed to parse: {error:?}")
                })
                .into_iter()
                .rev()
                .find_map(|element| match element {
                    BindingElement::Binding(binding) => Some(binding.trigger),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("default keybind {entry:?} has no binding trigger"))
        })
        .collect()
}

// Every shipped default keybind must parse; catches an invalid key token or action name (e.g. the
// PageUp/PageDown additions or a future typo) before it reaches users.
#[test]
fn default_keybind_tables_parse() {
    let tables: &[&[&str]] = &[
        common_keybinds_macos(),
        common_keybinds_other(),
        common_keybinds_windows(),
        native_keybinds(),
        native_scroll_keybinds_macos(),
        native_scroll_keybinds_other(),
        tmux_keybinds(),
    ];
    for table in tables {
        let mut set = BindingSet::default();
        for entry in *table {
            set.parse_and_put(entry).unwrap_or_else(|error| {
                panic!("default keybind {entry:?} failed to parse: {error:?}")
            });
        }
    }
}

// Root cause of the Linux/Windows breakage: a `cmd`/`super` binding maps to the Super/Windows key,
// which the desktop environment swallows. The non-macOS defaults must never require it.
#[test]
fn non_macos_default_keybinds_never_require_super() {
    let mut entries = Vec::new();
    entries.extend_from_slice(common_keybinds_other());
    entries.extend_from_slice(common_keybinds_windows());
    entries.extend_from_slice(native_scroll_keybinds_other());
    for trigger in binding_triggers(&entries) {
        assert!(
            !trigger.mods.command,
            "non-macOS default {trigger:?} requires the Super key"
        );
    }
}

// A repeated trigger silently shadows another action; the hand-authored tables must keep every
// trigger unique (the `cmd+w`/`cmd+shift+w` style collisions that motivated separate tables).
#[test]
fn common_keybind_triggers_are_unique_per_platform() {
    for table in [
        common_keybinds_macos(),
        common_keybinds_other(),
        common_keybinds_windows(),
    ] {
        let triggers = binding_triggers(table);
        for (index, trigger) in triggers.iter().enumerate() {
            assert!(
                !triggers[index + 1..].contains(trigger),
                "duplicate trigger {trigger:?} in default keybinds"
            );
        }
    }
}

// Known-answer: the Linux/Windows session shortcuts resolve to the WezTerm-style Ctrl+Shift combos.
#[test]
fn non_macos_session_shortcuts_use_ctrl_shift() {
    let mut set = BindingSet::default();
    for entry in common_keybinds_other() {
        set.parse_and_put(entry).unwrap();
    }
    assert_eq!(
        set.get_trigger(&BindingAction::NewMuxSession),
        Some(&BindingTrigger::from_str("ctrl+shift+n").unwrap())
    );
    assert_eq!(
        set.get_trigger(&BindingAction::SelectSession(1)),
        Some(&BindingTrigger::from_str("ctrl+shift+1").unwrap())
    );
    assert_eq!(
        set.get_trigger(&BindingAction::NextSession),
        Some(&BindingTrigger::from_str("ctrl+shift+]").unwrap())
    );
}

#[test]
fn windows_paste_defaults_include_standard_terminal_shortcuts() {
    let mut set = BindingSet::default();
    for entry in common_keybinds_windows() {
        set.parse_and_put(entry).unwrap();
    }

    assert_eq!(
        set.get(&BindingTrigger::from_str("ctrl+v").unwrap())
            .map(|binding| &binding.action),
        Some(&BindingAction::PasteFromClipboard)
    );
    assert_eq!(
        set.get(&BindingTrigger::from_str("ctrl+shift+v").unwrap())
            .map(|binding| &binding.action),
        Some(&BindingAction::PasteFromClipboard)
    );
    assert_eq!(
        set.get(&BindingTrigger::from_str("shift+Insert").unwrap())
            .map(|binding| &binding.action),
        Some(&BindingAction::PasteFromClipboard)
    );
}
