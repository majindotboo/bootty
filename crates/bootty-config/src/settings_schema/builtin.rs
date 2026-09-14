//! The built-in settings, as data.
//!
//! Adding a setting here is one entry. Its TOML path appears once, its fallback is read off the
//! default config rather than copied, and choice tokens use infallible enum conversions.
//! The schema round-trip tests verify that every choice agrees with the config loader.

use num_traits::ToPrimitive as _;

use crate::config::{
    MacosTitlebarStyle, OnLastWindowClosed, OpenBehavior, RestoreOnStartup, WhenClosingWithNoTabs,
    WindowDecoration, WindowFullscreen,
};

use super::{
    NumberControl, SettingDefault, SettingEditor, SettingKind, SettingOption, SettingSpec,
    SettingValue,
};

/// Build a spec with the fields every entry sets.
fn spec(
    path: &[&'static str],
    label: &'static str,
    help: &'static str,
    page: &'static str,
    section: &'static str,
    kind: SettingKind,
    default: SettingDefault,
) -> SettingSpec {
    SettingSpec {
        path: path.iter().map(|part| (*part).into()).collect(),
        label: label.into(),
        help: help.into(),
        page: page.into(),
        section: section.into(),
        kind,
        supersedes: Vec::new(),
        default,
    }
}

fn number(
    range: std::ops::RangeInclusive<f32>,
    control: NumberControl,
    suffix: &'static str,
) -> SettingKind {
    SettingKind::Number {
        range,
        control,
        precision: 2,
        suffix: suffix.into(),
        display_scale: 1.0,
    }
}

fn text(placeholder: &'static str, optional: bool) -> SettingKind {
    SettingKind::Text {
        placeholder: placeholder.into(),
        optional,
    }
}

/// A 0.0-1.0 fraction the user edits as a percentage.
fn fraction(control: NumberControl) -> SettingKind {
    SettingKind::Number {
        range: 0.0..=1.0,
        control,
        precision: 2,
        suffix: "%".into(),
        display_scale: 100.0,
    }
}

fn custom(
    path: &[&'static str],
    page: &'static str,
    section: &'static str,
    editor: SettingEditor,
) -> SettingSpec {
    SettingSpec {
        path: path.iter().map(|part| (*part).into()).collect(),
        label: String::new().into(),
        help: String::new().into(),
        page: page.into(),
        section: section.into(),
        kind: SettingKind::Custom(editor),
        supersedes: Vec::new(),
        default: SettingDefault::Unused,
    }
}

/// Every accepted compatibility spelling that is not itself a settings-surface control.
pub(super) const fn compatibility_paths() -> &'static [&'static [&'static str]] {
    &[
        // Retired panel preferences remain loadable but have no UI or runtime effect.
        &["panels", "jobs", "dock"],
        &["panels", "jobs", "button"],
        &["panels", "transfers", "dock"],
        &["panels", "transfers", "button"],
        &["panels", "recovery", "dock"],
        &["panels", "recovery", "button"],
        &["panels", "shell", "dock"],
        &["panels", "shell", "button"],
        &["font-feature"],
        &["chrome", "status-bar"],
        &["chrome", "status-segment"],
        &["chrome", "window-tabs"],
        &["chrome", "sidebar"],
        &["chrome", "sidebar-width"],
        &["sidebar", "position"],
        &["sidebar", "fullscreen-background"],
        &["sidebar", "fullscreen-hover"],
        &["multiplexer", "herdr-session"],
        &["auto_update"],
    ]
}

pub(super) fn specs() -> Vec<SettingSpec> {
    let mut specs = Vec::new();
    specs.extend(dock_header_specs());
    specs.extend(dock_tabs_specs());
    specs.extend(terminal_tabs_specs());
    specs.extend(interface_preferences_specs());
    specs.extend(application_lifecycle_specs());
    specs.extend(open_behavior_specs());
    specs.extend(window_chrome_specs());
    specs.extend(fullscreen_specs());
    specs.extend(background_image_specs());
    specs.extend(background_effects_specs());
    specs.extend(window_size_specs());
    specs.extend(pane_appearance_specs());
    specs.extend(font_metrics_specs());
    specs.extend(terminal_integration_specs());
    specs.extend(notifications_specs());
    specs.extend(terminal_environment_specs());
    specs.extend(status_bar_specs());
    specs.extend(diagnostics_specs());
    specs.extend(themes_specs());
    specs.extend(cursor_specs());
    specs.extend(font_styles_specs());
    specs.extend(custom_chrome_specs());
    specs.extend(sidebar_specs());
    specs.extend(backend_specs());
    specs.extend(ssh_profiles_specs());
    specs.extend(input_specs());
    specs.extend(custom_runtime_specs());
    specs.extend(font_weight_specs());
    specs.extend(panel_specs());
    specs
}

fn token<T: Copy + Into<&'static str>>(value: &T) -> String {
    let token: &'static str = (*value).into();
    token.to_owned()
}

fn font_weight_specs() -> impl Iterator<Item = SettingSpec> {
    crate::FontWeightRole::ALL.into_iter().map(|role| SettingSpec {
            path: vec!["font".into(), "ui-weights".into(), token(&role).into()],
            label: format!("{} style", role.label()).into(),
            help: "Automatic keeps this interface weight relative to the base font. A named style overrides it.".into(),
            page: "text".into(),
            section: "FONT".into(),
            kind: SettingKind::FontStyle,
            supersedes: Vec::new(),
            default: SettingDefault::UiFontWeight(role),
    })
}

fn panel_specs() -> Vec<SettingSpec> {
    let mut specs = Vec::new();
    macro_rules! panel {
        ($kind:ident, $name:literal, $label:literal) => {
            specs.push(spec(
                &["panels", $name, "dock"],
                "Dock",
                "Where this panel opens.",
                "panels",
                $label,
                SettingKind::Choice {
                    options: [
                        (&crate::config::PanelDock::Left, "Left"),
                        (&crate::config::PanelDock::Right, "Right"),
                        (&crate::config::PanelDock::Bottom, "Bottom"),
                    ]
                    .into_iter()
                    .map(|(value, label)| SettingOption::of(value, label))
                    .collect(),
                },
                SettingDefault::Field(|config| {
                    SettingValue::Token(token(
                        &config
                            .panel(crate::config::PanelKind::$kind)
                            .dock(crate::config::PanelKind::$kind),
                    ))
                }),
            ));
            specs.push(spec(
                &["panels", $name, "button"],
                "Status bar button",
                "Show a button that toggles this panel.",
                "panels",
                $label,
                SettingKind::Choice {
                    options: [
                        (&crate::config::PanelButton::None, "None"),
                        (&crate::config::PanelButton::Top, "Top bar"),
                        (&crate::config::PanelButton::Bottom, "Bottom bar"),
                    ]
                    .into_iter()
                    .map(|(value, label)| SettingOption::of(value, label))
                    .collect(),
                },
                SettingDefault::Field(|config| {
                    SettingValue::Token(token(
                        &config.panel(crate::config::PanelKind::$kind).button,
                    ))
                }),
            ));
        };
    }
    panel!(Sessions, "sessions", "Sessions");
    panel!(Files, "files", "Files");
    panel!(Changes, "changes", "Changes");
    panel!(Diff, "diff", "Diff");
    panel!(Agents, "agents", "Agents");
    specs
}

fn dock_header_specs() -> [SettingSpec; 4] {
    [
        spec(
            &["chrome", "left-dock-toggle"],
            "Show left dock button",
            "Hide the header button while keeping the dock command available.",
            "appearance",
            "DOCKS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.chrome.left_dock_toggle)),
        ),
        spec(
            &["chrome", "right-dock-toggle"],
            "Show right dock button",
            "Hide the header button while keeping the dock command available.",
            "appearance",
            "DOCKS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.chrome.right_dock_toggle)),
        ),
        spec(
            &["chrome", "panel-tab-style"],
            "Fixed dock tab labels",
            "Label style for tabs in the narrow left and right docks. Main Dock tabs use icons and text.",
            "appearance",
            "DOCKS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::described(
                        &crate::config::PanelTabStyle::Icons,
                        "Icons only",
                        "Show icons; hover for panel names.",
                    ),
                    SettingOption::described(
                        &crate::config::PanelTabStyle::IconsAndText,
                        "Icons and text",
                        "Show each panel’s icon and name.",
                    ),
                    SettingOption::described(
                        &crate::config::PanelTabStyle::Text,
                        "Text only",
                        "Show panel names without icons.",
                    ),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.chrome.panel_tab_style))
            }),
        ),
        spec(
            &["chrome", "panel-tabs"],
            "Fixed dock tabs",
            "Tab visibility for the fixed left and right docks. Main and bottom Dock tabs stay visible unless hidden for that group.",
            "appearance",
            "DOCKS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::described(
                        &crate::config::PanelTabs::Automatic,
                        "Hide for a single panel",
                        "Show tabs only when a group has multiple panels.",
                    ),
                    SettingOption::described(
                        &crate::config::PanelTabs::Always,
                        "Always show",
                        "Keep tabs visible even for a single panel.",
                    ),
                    SettingOption::described(
                        &crate::config::PanelTabs::Never,
                        "Always hide",
                        "Hide tabs and switch panels with commands.",
                    ),
                ],
            },
            SettingDefault::Field(|config| SettingValue::Token(token(&config.chrome.panel_tabs))),
        ),
    ]
}

fn dock_tabs_specs() -> [SettingSpec; 3] {
    [
        spec(
            &["chrome", "dock-tabs", "appearance"],
            "Tab style",
            "Choose the appearance of dock tabs.",
            "panels",
            "DOCK TABS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::TabAppearance::Classic, "Classic"),
                    SettingOption::of(&crate::config::TabAppearance::Underline, "Underline"),
                    SettingOption::of(&crate::config::TabAppearance::Pill, "Pill"),
                    SettingOption::of(&crate::config::TabAppearance::Outline, "Outline"),
                    SettingOption::of(&crate::config::TabAppearance::Segmented, "Segmented"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.chrome.dock_tabs.appearance))
            }),
        ),
        spec(
            &["chrome", "dock-tabs", "close-position"],
            "Close button side",
            "Place the close button on the left or right side of each tab.",
            "panels",
            "DOCK TABS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::TabClosePosition::Left, "Left"),
                    SettingOption::of(&crate::config::TabClosePosition::Right, "Right"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.chrome.dock_tabs.close_position))
            }),
        ),
        spec(
            &["chrome", "dock-tabs", "close-button"],
            "Show close button",
            "Show close buttons always, on hover, or never.",
            "panels",
            "DOCK TABS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::TabCloseButton::Always, "Always"),
                    SettingOption::of(&crate::config::TabCloseButton::Hover, "On hover"),
                    SettingOption::of(&crate::config::TabCloseButton::Hidden, "Hidden"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.chrome.dock_tabs.close_button))
            }),
        ),
    ]
}

fn terminal_tabs_specs() -> [SettingSpec; 3] {
    [
        spec(
            &["chrome", "terminal-tabs", "appearance"],
            "Tab style",
            "Choose the appearance of terminal tabs.",
            "panels",
            "TERMINAL TABS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::TabAppearance::Classic, "Classic"),
                    SettingOption::of(&crate::config::TabAppearance::Underline, "Underline"),
                    SettingOption::of(&crate::config::TabAppearance::Pill, "Pill"),
                    SettingOption::of(&crate::config::TabAppearance::Outline, "Outline"),
                    SettingOption::of(&crate::config::TabAppearance::Segmented, "Segmented"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.chrome.terminal_tabs.appearance))
            }),
        ),
        spec(
            &["chrome", "terminal-tabs", "close-position"],
            "Close button side",
            "Place the close button on the left or right side of each tab.",
            "panels",
            "TERMINAL TABS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::TabClosePosition::Left, "Left"),
                    SettingOption::of(&crate::config::TabClosePosition::Right, "Right"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.chrome.terminal_tabs.close_position))
            }),
        ),
        spec(
            &["chrome", "terminal-tabs", "close-button"],
            "Show close button",
            "Show close buttons always, on hover, or never.",
            "panels",
            "TERMINAL TABS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::TabCloseButton::Always, "Always"),
                    SettingOption::of(&crate::config::TabCloseButton::Hover, "On hover"),
                    SettingOption::of(&crate::config::TabCloseButton::Hidden, "Hidden"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.chrome.terminal_tabs.close_button))
            }),
        ),
    ]
}

fn interface_preferences_specs() -> [SettingSpec; 2] {
    [
        spec(
            &["locale"],
            "Language",
            "Interface language tag. English (en) is the fallback; en-XA previews longer translated labels. Restart to update native menus.",
            "general",
            "LANGUAGE",
            text("en", false),
            SettingDefault::Field(|config| SettingValue::Text(config.locale.clone())),
        ),
        spec(
            &["session", "scrollbar"],
            "Show scrollbar",
            "Show the terminal scrollbar while scrolling, on hover, always, or never.",
            "shell",
            "SCROLLBACK",
            SettingKind::Choice {
                options: vec![
                    SettingOption::described(
                        &crate::config::TerminalScrollbar::Auto,
                        "Auto",
                        "Show while scrolling and hide when idle.",
                    ),
                    SettingOption::described(
                        &crate::config::TerminalScrollbar::Hover,
                        "On hover",
                        "Show when the pointer is over the scrollbar edge.",
                    ),
                    SettingOption::described(
                        &crate::config::TerminalScrollbar::Always,
                        "Always",
                        "Show whenever scrollback is available.",
                    ),
                    SettingOption::described(
                        &crate::config::TerminalScrollbar::Never,
                        "Never",
                        "Hide the scrollbar; wheel and keyboard scrolling still work.",
                    ),
                ],
            },
            SettingDefault::Field(|config| SettingValue::Token(token(&config.session.scrollbar))),
        ),
    ]
}

fn application_lifecycle_specs() -> [SettingSpec; 2] {
    [
        spec(
            &["restore_on_startup"],
            "Restore on startup",
            "Choose which persisted Space selection Bootty restores when it starts.",
            "general",
            "STARTUP",
            SettingKind::Choice {
                options: vec![
                    SettingOption::described(
                        &RestoreOnStartup::LastSession,
                        "Last session",
                        "Restore the Space selected for this application window.",
                    ),
                    SettingOption::described(
                        &RestoreOnStartup::LastWorkspace,
                        "Last workspace",
                        "Restore the Space selected in the primary Bootty window.",
                    ),
                    SettingOption::described(
                        &RestoreOnStartup::None,
                        "None",
                        "Open the first persisted Space instead of restoring a selection.",
                    ),
                ],
            },
            SettingDefault::Field(|config| SettingValue::Token(token(&config.restore_on_startup))),
        ),
        spec(
            &["on_last_window_closed"],
            "On last window closed",
            "Choose whether closing the last Bootty window also quits the application.",
            "general",
            "WINDOWS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::described(
                        &OnLastWindowClosed::PlatformDefault,
                        "Platform default",
                        "Follow the operating system convention.",
                    ),
                    SettingOption::described(
                        &OnLastWindowClosed::QuitApp,
                        "Quit application",
                        "Quit Bootty after its last window closes.",
                    ),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.on_last_window_closed))
            }),
        ),
    ]
}

fn open_behavior_specs() -> [SettingSpec; 3] {
    [
        spec(
            &["cli_default_open_behavior"],
            "CLI open behavior",
            "Choose whether a second Bootty CLI launch reuses the running window or opens another one.",
            "general",
            "WINDOWS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&OpenBehavior::ExistingWindow, "Existing window"),
                    SettingOption::of(&OpenBehavior::NewWindow, "New window"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.cli_default_open_behavior))
            }),
        ),
        spec(
            &["default_open_behavior"],
            "Space open behavior",
            "Choose whether opening a Space from the UI reuses this window or opens another one.",
            "general",
            "WINDOWS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&OpenBehavior::ExistingWindow, "Existing window"),
                    SettingOption::of(&OpenBehavior::NewWindow, "New window"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.default_open_behavior))
            }),
        ),
        spec(
            &["when_closing_with_no_tabs"],
            "When closing with no tabs",
            "Choose what close active item does when the terminal has no remaining tab to close.",
            "general",
            "WINDOWS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&WhenClosingWithNoTabs::PlatformDefault, "Platform default"),
                    SettingOption::of(&WhenClosingWithNoTabs::CloseWindow, "Close window"),
                    SettingOption::of(&WhenClosingWithNoTabs::KeepWindowOpen, "Keep window open"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.when_closing_with_no_tabs))
            }),
        ),
    ]
}

fn window_chrome_specs() -> [SettingSpec; 3] {
    [
        spec(
            &["window", "title"],
            "Title",
            "Shown in native window chrome.",
            "window",
            "WINDOW",
            text("Bootty", false),
            SettingDefault::Field(|config| SettingValue::Text(config.window.title.clone())),
        ),
        spec(
            &["window", "macos-titlebar-style"],
            "Titlebar style",
            "macOS window chrome treatment.",
            "window",
            "WINDOW",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&MacosTitlebarStyle::Native, "System titlebar"),
                    SettingOption::of(&MacosTitlebarStyle::Transparent, "Transparent"),
                    SettingOption::of(&MacosTitlebarStyle::Hidden, "Hidden"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.window.macos_titlebar_style))
            }),
        ),
        spec(
            &["window", "window-decoration"],
            "Decoration",
            "Choose who draws the outer window border.",
            "window",
            "WINDOW",
            SettingKind::Choice {
                options: vec![
                    SettingOption::described(
                        &WindowDecoration::Auto,
                        "Automatic",
                        "Let the platform pick based on the titlebar style.",
                    ),
                    SettingOption::described(
                        &WindowDecoration::None,
                        "Borderless",
                        "No outer border or system window controls.",
                    ),
                    SettingOption::described(
                        &WindowDecoration::Client,
                        "Drawn by Bootty",
                        "Bootty paints the window border and controls.",
                    ),
                    SettingOption::described(
                        &WindowDecoration::Server,
                        "Drawn by system",
                        "The OS paints the native window border.",
                    ),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.window.window_decoration))
            }),
        ),
    ]
}

fn fullscreen_specs() -> [SettingSpec; 2] {
    [
        spec(
            &["window", "fullscreen-enabled"],
            "Fullscreen on launch",
            "Start Bootty in the selected fullscreen style.",
            "window",
            "WINDOW",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.window.fullscreen_enabled)),
        ),
        spec(
            &["window", "fullscreen"],
            "Fullscreen style",
            "Selects native fullscreen or a notch-aware borderless mode.",
            "window",
            "WINDOW",
            SettingKind::Choice {
                options: vec![
                    SettingOption::described(
                        &WindowFullscreen::Native,
                        "Native",
                        "Use macOS native Spaces fullscreen.",
                    ),
                    SettingOption::described(
                        &WindowFullscreen::NonNative,
                        "Borderless",
                        "Fill the display without native Spaces.",
                    ),
                    SettingOption::described(
                        &WindowFullscreen::NonNativeVisibleMenu,
                        "Borderless + menu bar",
                        "Keep the menu bar visible in borderless fullscreen.",
                    ),
                    SettingOption::described(
                        &WindowFullscreen::NonNativePaddedNotch,
                        "Borderless + notch padding",
                        "Reserve space for a notched display.",
                    ),
                ],
            },
            SettingDefault::Field(|config| SettingValue::Token(token(&config.window.fullscreen))),
        ),
    ]
}

fn background_image_specs() -> [SettingSpec; 3] {
    [
        spec(
            &["window", "background-opacity"],
            "Terminal background opacity",
            "Text and explicit application colors remain opaque.",
            "appearance",
            "BACKGROUND",
            fraction(NumberControl::Slider),
            SettingDefault::Field(|config| SettingValue::Number(config.window.background_opacity)),
        ),
        spec(
            &["window", "background-image"],
            "Background image",
            "Local path, relative to the configuration directory. Lower terminal background opacity to reveal it.",
            "appearance",
            "BACKGROUND",
            text("Local image path", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(
                    config
                        .window
                        .background_image
                        .as_ref()
                        .map(|path| path.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            }),
        ),
        spec(
            &["window", "background-image-opacity"],
            "Image opacity",
            "Blend the image over the gradient or desktop.",
            "appearance",
            "BACKGROUND",
            fraction(NumberControl::Slider),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.window.background_image_opacity)
            }),
        ),
    ]
}

fn background_effects_specs() -> [SettingSpec; 4] {
    [
        spec(
            &["window", "background-gradient-start"],
            "Gradient start",
            "Optional #RRGGBB or #RRGGBBAA. Both ends enable the gradient.",
            "appearance",
            "BACKGROUND",
            text("#RRGGBBAA", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(
                    config
                        .window
                        .background_gradient_start
                        .map(|color| {
                            format!(
                                "#{:02X}{:02X}{:02X}{:02X}",
                                color.r, color.g, color.b, color.a
                            )
                        })
                        .unwrap_or_default(),
                )
            }),
        ),
        spec(
            &["window", "background-gradient-end"],
            "Gradient end",
            "Optional #RRGGBB or #RRGGBBAA. Both ends enable the gradient.",
            "appearance",
            "BACKGROUND",
            text("#RRGGBBAA", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(
                    config
                        .window
                        .background_gradient_end
                        .map(|color| {
                            format!(
                                "#{:02X}{:02X}{:02X}{:02X}",
                                color.r, color.g, color.b, color.a
                            )
                        })
                        .unwrap_or_default(),
                )
            }),
        ),
        spec(
            &["window", "background-gradient-angle"],
            "Gradient angle",
            "Clockwise degrees, from 0 to 360.",
            "appearance",
            "BACKGROUND",
            number(0.0..=360.0, NumberControl::Edit, "°"),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.window.background_gradient_angle)
            }),
        ),
        spec(
            &["window", "background-material"],
            "Window material",
            "Blur depends on the compositor; Mica requires Windows 11. Other platforms fall back to transparency.",
            "appearance",
            "BACKGROUND",
            SettingKind::Choice {
                options: [
                    (crate::config::BackgroundMaterial::Opaque, "Opaque"),
                    (
                        crate::config::BackgroundMaterial::Transparent,
                        "Transparent",
                    ),
                    (crate::config::BackgroundMaterial::Blurred, "Blurred"),
                    (crate::config::BackgroundMaterial::Mica, "Mica"),
                    (crate::config::BackgroundMaterial::MicaAlt, "Mica Alt"),
                ]
                .into_iter()
                .map(|(value, label)| SettingOption::of(&value, label))
                .collect(),
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.window.background_material))
            }),
        ),
    ]
}

fn window_size_specs() -> [SettingSpec; 3] {
    [
        spec(
            &["window", "width"],
            "Width",
            "Applies to newly created windows.",
            "window",
            "DEFAULT SIZE",
            number(400.0..=6000.0, NumberControl::Edit, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.window.width)),
        ),
        spec(
            &["window", "height"],
            "Height",
            "Applies to newly created windows.",
            "window",
            "DEFAULT SIZE",
            number(300.0..=4000.0, NumberControl::Edit, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.window.height)),
        ),
        spec(
            &["window", "fullscreen-tabs-in-notch"],
            "Tabs in notch band",
            "Allow terminal chrome to occupy the notch/menu-bar band.",
            "window",
            "FULLSCREEN NOTCH",
            SettingKind::Bool,
            SettingDefault::Field(|config| {
                SettingValue::Bool(config.window.fullscreen_tabs_in_notch)
            }),
        ),
    ]
}

fn pane_appearance_specs() -> [SettingSpec; 6] {
    [
        spec(
            &["chrome", "gap"],
            "Chrome gap",
            "Spacing between sidebar, status, and terminal content.",
            "window",
            "CHROME",
            number(0.0..=24.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.gap)),
        ),
        spec(
            &["chrome", "unfocused-sidebar-dim"],
            "Inactive sidebar dim",
            "Opacity reduction when the window is not focused.",
            "window",
            "CHROME",
            fraction(NumberControl::Slider),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.chrome.unfocused_sidebar_dim)
            }),
        ),
        spec(
            &["chrome", "unfocused-terminal-dim"],
            "Inactive terminal dim",
            "Dark overlay applied to unfocused split panes.",
            "window",
            "CHROME",
            fraction(NumberControl::Slider),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.chrome.unfocused_terminal_dim)
            }),
        ),
        spec(
            &["chrome", "pane-divider-width"],
            "Divider width",
            "Thickness of the divider between split panes.",
            "window",
            "SPLIT PANES",
            number(0.0..=16.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.pane_divider_width)),
        ),
        spec(
            &["chrome", "pane-focus-border-width"],
            "Focus border width",
            "Border drawn around the focused split pane (0 hides it).",
            "window",
            "SPLIT PANES",
            number(0.0..=8.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| {
                SettingValue::Number(config.chrome.pane_focus_border_width)
            }),
        ),
        spec(
            &["chrome", "pane-corner-radius"],
            "Corner radius",
            "Rounding of split pane corners.",
            "window",
            "SPLIT PANES",
            number(0.0..=40.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.pane_corner_radius)),
        ),
    ]
}

fn font_metrics_specs() -> [SettingSpec; 7] {
    [
        spec(
            &["font", "size"],
            "Font size",
            "Main terminal text size.",
            "text",
            "TERMINAL METRICS",
            number(6.0..=48.0, NumberControl::Slider, "pt"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.size)),
        ),
        spec(
            &["font", "ui-size"],
            "UI font size",
            "Text size for Bootty chrome, extensions, the sidebar, and the status bar.",
            "text",
            "FONT",
            number(6.0..=48.0, NumberControl::Slider, "px"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.ui_size)),
        ),
        spec(
            &["font", "fit-cell-height"],
            "Fit rows to window",
            "Stretch row spacing so terminal content fills available height.",
            "text",
            "TERMINAL METRICS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.font.fit_cell_height)),
        ),
        spec(
            &["font", "fit-cell-width"],
            "Fit columns to window",
            "Stretch column spacing so terminal content fills available width (avoids a gap on the right, common with split panes).",
            "text",
            "TERMINAL METRICS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.font.fit_cell_width)),
        ),
        spec(
            &["font", "baseline-adjustment"],
            "Baseline adjustment",
            "Move glyphs up or down in logical pixels; positive values move them up.",
            "text",
            "GLYPH BEHAVIOR",
            number(-12.0..=12.0, NumberControl::Slider, "px"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.baseline_adjustment)),
        ),
        spec(
            &["font", "underline-position"],
            "Underline position",
            "Tune where underline decoration is drawn.",
            "text",
            "GLYPH BEHAVIOR",
            number(-12.0..=12.0, NumberControl::Slider, "px"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.underline_position)),
        ),
        spec(
            &["font", "underline-thickness"],
            "Underline thickness",
            "Tune underline stroke thickness.",
            "text",
            "GLYPH BEHAVIOR",
            number(0.0..=8.0, NumberControl::Slider, "px"),
            SettingDefault::Field(|config| SettingValue::Number(config.font.underline_thickness)),
        ),
    ]
}

fn terminal_integration_specs() -> [SettingSpec; 4] {
    [
        spec(
            &["session", "shell"],
            "Shell",
            "Empty uses the macOS account login shell. Applies to new sessions.",
            "shell",
            "SHELL",
            text("default login shell", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(config.session.shell.clone().unwrap_or_default())
            }),
        ),
        spec(
            &["session", "shell-integration"],
            "Shell integration",
            "Add command lifecycle and directory hooks to new supported shells. Does not change shell editing or rc files.",
            "shell",
            "SHELL",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.session.shell_integration)),
        ),
        spec(
            &["session", "output-archives"],
            "Save terminal output",
            "Checkpoint attached panes every 30 seconds for previous-session browsing. May retain sensitive output. Keeps 32 archives per window.",
            "shell",
            "RECOVERY",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.session.output_archives)),
        ),
        spec(
            &["session", "clipboard-write-hosts"],
            "Image clipboard hosts",
            "Space-separated host keys allowed to replace the clipboard: local, ssh:user@host:port, wsl:distribution. Empty denies all.",
            "shell",
            "CLIPBOARD",
            text("", false),
            SettingDefault::Field(|config| {
                SettingValue::Text(config.session.clipboard_write_hosts.clone())
            }),
        ),
    ]
}

fn notifications_specs() -> [SettingSpec; 4] {
    [
        spec(
            &["session", "bell"],
            "Terminal bell",
            "Choose visual, system audio, both, or no bell.",
            "shell",
            "NOTIFICATIONS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::BellMode::Off, "Off"),
                    SettingOption::of(&crate::config::BellMode::Visual, "Visual"),
                    SettingOption::of(&crate::config::BellMode::Audio, "Audio"),
                    SettingOption::of(&crate::config::BellMode::Both, "Both"),
                ],
            },
            SettingDefault::Field(|config| SettingValue::Token(token(&config.session.bell))),
        ),
        spec(
            &["session", "agent-notifications"],
            "Agent attention",
            "Notify when an agent completes, needs input or reports an error.",
            "general",
            "NOTIFICATIONS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::NotificationPolicy::Never, "Never"),
                    SettingOption::of(
                        &crate::config::NotificationPolicy::Unfocused,
                        "When unfocused",
                    ),
                    SettingOption::of(&crate::config::NotificationPolicy::Always, "Always"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.session.agent_notifications))
            }),
        ),
        spec(
            &["session", "command-notifications"],
            "Command finished",
            "Notify when a shell-reported command finishes. Unfocused includes another pane or Space.",
            "shell",
            "NOTIFICATIONS",
            SettingKind::Choice {
                options: vec![
                    SettingOption::of(&crate::config::NotificationPolicy::Never, "Never"),
                    SettingOption::of(
                        &crate::config::NotificationPolicy::Unfocused,
                        "When unfocused",
                    ),
                    SettingOption::of(&crate::config::NotificationPolicy::Always, "Always"),
                ],
            },
            SettingDefault::Field(|config| {
                SettingValue::Token(token(&config.session.command_notifications))
            }),
        ),
        spec(
            &["session", "command-notification-min-seconds"],
            "Minimum command duration",
            "Only notify for commands lasting at least this many seconds.",
            "shell",
            "NOTIFICATIONS",
            SettingKind::Number {
                range: 0.0..=86400.0,
                control: NumberControl::Edit,
                precision: 0,
                suffix: "s".into(),
                display_scale: 1.0,
            },
            SettingDefault::Field(|config| {
                SettingValue::Number(
                    config
                        .session
                        .command_notification_min_seconds
                        .to_f32()
                        .unwrap_or(f32::MAX),
                )
            }),
        ),
    ]
}

fn terminal_environment_specs() -> [SettingSpec; 4] {
    [
        spec(
            &["session", "working-directory"],
            "Working directory",
            "Empty starts new sessions in your home directory.",
            "shell",
            "SHELL",
            text("inherit from launcher", true),
            SettingDefault::Field(|config| {
                SettingValue::Text(
                    config
                        .session
                        .working_directory
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_default(),
                )
            }),
        ),
        spec(
            &["session", "term"],
            "TERM",
            "Advertised terminal type for new shells.",
            "shell",
            "TERMINAL IDENTITY",
            text("xterm-256color", true),
            SettingDefault::Field(|config| SettingValue::Text(config.session.term.clone())),
        ),
        spec(
            &["session", "colorterm"],
            "COLORTERM",
            "Advertised color capability for new shells.",
            "shell",
            "TERMINAL IDENTITY",
            text("truecolor", true),
            SettingDefault::Field(|config| SettingValue::Text(config.session.colorterm.clone())),
        ),
        spec(
            &["session", "glyph-protocol"],
            "Glyph protocol",
            "Expose terminal image/glyph protocol support to new sessions.",
            "shell",
            "TERMINAL IDENTITY",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.session.glyph_protocol)),
        ),
    ]
}

fn status_bar_specs() -> [SettingSpec; 3] {
    [
        spec(
            &["chrome", "bottom-bar"],
            "Bottom bar",
            "Show the module bar below the terminal.",
            "status",
            "BARS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.chrome.bottom_bar)),
        ),
        spec(
            &["chrome", "status-height"],
            "Height",
            "Module strip height.",
            "status",
            "STATUS BARS",
            number(20.0..=80.0, NumberControl::Slider, " px"),
            SettingDefault::Field(|config| SettingValue::Number(config.chrome.status_height)),
        ),
        spec(
            &["multiplexer", "hide-tmux-status"],
            "Hide tmux's own bar",
            "Avoid duplicate status bars when the tmux backend is active.",
            "status",
            "STATUS BARS",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.multiplexer.hide_tmux_status)),
        ),
    ]
}

fn diagnostics_specs() -> [SettingSpec; 1] {
    [spec(
        &["diagnostics", "stability-trace"],
        "Stability trace",
        "Writes frame-timing diagnostics to this file. Leave empty to disable.",
        "diagnostics",
        "TRACE",
        text("path to trace log", true),
        SettingDefault::Field(|config| {
            SettingValue::Text(
                config
                    .diagnostics
                    .stability_trace
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_default(),
            )
        }),
    )]
}

fn themes_specs() -> [SettingSpec; 7] {
    [
        custom(&["theme"], "colors", "THEME", SettingEditor::Colors),
        custom(&["colors", "*"], "colors", "COLORS", SettingEditor::Colors),
        custom(
            &["appearance", "mode"],
            "colors",
            "COLOR MODE",
            SettingEditor::Colors,
        ),
        custom(
            &["appearance", "light", "theme"],
            "colors",
            "THEME",
            SettingEditor::Colors,
        ),
        custom(
            &["appearance", "light", "colors", "*"],
            "colors",
            "COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["appearance", "dark", "theme"],
            "colors",
            "THEME",
            SettingEditor::Colors,
        ),
        custom(
            &["appearance", "dark", "colors", "*"],
            "colors",
            "COLORS",
            SettingEditor::Colors,
        ),
    ]
}

fn cursor_specs() -> [SettingSpec; 3] {
    [
        custom(
            &["cursor", "style"],
            "appearance",
            "CURSOR",
            SettingEditor::Appearance,
        ),
        spec(
            &["cursor", "blink"],
            "Blink cursor",
            "Make the default cursor blink.",
            "appearance",
            "CURSOR",
            SettingKind::Bool,
            SettingDefault::Field(|config| {
                SettingValue::Bool(config.cursor.blink.unwrap_or(false))
            }),
        ),
        spec(
            &["cursor", "dim-inactive-pane"],
            "Dim inactive pane cursors",
            "Keep cursors steady and slightly dimmer outside the focused pane.",
            "appearance",
            "CURSOR",
            SettingKind::Bool,
            SettingDefault::Field(|config| SettingValue::Bool(config.cursor.dim_inactive_pane)),
        ),
    ]
}

fn font_styles_specs() -> [SettingSpec; 9] {
    [
        custom(&["font", "family"], "text", "FONT", SettingEditor::Text),
        spec(
            &["font", "style-bold"],
            "Bold style",
            "Automatic follows the base weight. Choose a named font style to override bold text.",
            "text",
            "FONT",
            SettingKind::FontStyle,
            SettingDefault::Field(|config| (&config.font.style_bold).into()),
        ),
        spec(
            &["font", "style-italic"],
            "Italic style",
            "Automatic keeps the base weight and selects its italic style.",
            "text",
            "FONT",
            SettingKind::FontStyle,
            SettingDefault::Field(|config| (&config.font.style_italic).into()),
        ),
        spec(
            &["font", "style-bold-italic"],
            "Bold italic style",
            "Automatic follows the base weight and selects an italic style for bold italic text.",
            "text",
            "FONT",
            SettingKind::FontStyle,
            SettingDefault::Field(|config| (&config.font.style_bold_italic).into()),
        ),
        custom(&["font", "ui-family"], "text", "FONT", SettingEditor::Text),
        custom(
            &["font", "ui-use-terminal-family"],
            "text",
            "FONT",
            SettingEditor::Text,
        ),
        custom(
            &["font", "features"],
            "text",
            "FEATURES",
            SettingEditor::Text,
        ),
        custom(
            &["font", "cell-width"],
            "text",
            "TERMINAL METRICS",
            SettingEditor::Text,
        ),
        custom(
            &["font", "cell-height"],
            "text",
            "TERMINAL METRICS",
            SettingEditor::Text,
        ),
    ]
}

fn custom_chrome_specs() -> [SettingSpec; 7] {
    [
        custom(
            &["chrome", "top-bar"],
            "status",
            "BARS",
            SettingEditor::Status,
        ),
        custom(
            &["chrome", "status-background"],
            "colors",
            "CHROME COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["chrome", "pane-divider-color"],
            "colors",
            "CHROME COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["chrome", "notched-fullscreen-black-chrome"],
            "appearance",
            "FULLSCREEN NOTCH",
            SettingEditor::Appearance,
        ),
        custom(
            &["chrome", "pane-focus-border-color"],
            "colors",
            "CHROME COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["chrome", "top-segment"],
            "status",
            "SEGMENTS",
            SettingEditor::Status,
        ),
        custom(
            &["chrome", "bottom-segment"],
            "status",
            "SEGMENTS",
            SettingEditor::Status,
        ),
    ]
}

fn sidebar_specs() -> [SettingSpec; 7] {
    [
        custom(
            &["sidebar", "background"],
            "colors",
            "SIDEBAR COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["sidebar", "foreground"],
            "colors",
            "SIDEBAR COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["sidebar", "selected"],
            "colors",
            "SIDEBAR COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["sidebar", "hover"],
            "colors",
            "SIDEBAR COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["sidebar", "border"],
            "colors",
            "SIDEBAR COLORS",
            SettingEditor::Colors,
        ),
        custom(
            &["sidebar", "session-modules"],
            "sidebar",
            "MODULES",
            SettingEditor::Sidebar,
        ),
        custom(
            &["sidebar", "modules"],
            "sidebar",
            "MODULES",
            SettingEditor::Sidebar,
        ),
    ]
}

fn backend_specs() -> [SettingSpec; 7] {
    [
        custom(
            &["multiplexer", "backend"],
            "general",
            "MULTIPLEXER",
            SettingEditor::General,
        ),
        custom(
            &["multiplexer", "remote", "distribution"],
            "remotes",
            "DEFAULT REMOTE",
            SettingEditor::Remotes,
        ),
        custom(
            &["multiplexer", "remote", "host"],
            "remotes",
            "DEFAULT REMOTE",
            SettingEditor::Remotes,
        ),
        custom(
            &["multiplexer", "remote", "user"],
            "remotes",
            "DEFAULT REMOTE",
            SettingEditor::Remotes,
        ),
        custom(
            &["multiplexer", "remote", "port"],
            "remotes",
            "DEFAULT REMOTE",
            SettingEditor::Remotes,
        ),
        custom(
            &["multiplexer", "remote", "program"],
            "remotes",
            "DEFAULT REMOTE",
            SettingEditor::Remotes,
        ),
        custom(
            &["multiplexer", "remote", "args"],
            "remotes",
            "DEFAULT REMOTE",
            SettingEditor::Remotes,
        ),
    ]
}

fn ssh_profiles_specs() -> [SettingSpec; 10] {
    [
        custom(
            &["ssh-profiles", "*", "name"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "host"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "user"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "port"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "authentication"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "host-key-policy"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "identity-file"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "proxy-jump"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "program"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
        custom(
            &["ssh-profiles", "*", "args"],
            "remotes",
            "SSH PROFILES",
            SettingEditor::Remotes,
        ),
    ]
}

fn input_specs() -> [SettingSpec; 12] {
    [
        custom(
            &["input", "modifier-remap"],
            "keys",
            "INPUT",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "macos-option-as-alt"],
            "keys",
            "INPUT",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "hide-mouse-pointer-while-typing"],
            "appearance",
            "MOUSE POINTER",
            SettingEditor::Appearance,
        ),
        custom(
            &["input", "copy-on-select"],
            "keys",
            "INPUT",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "preset"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "prefix"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "keybind"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "sidebar-keybind"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "backend-keybind", "herdr"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "backend-keybind", "native"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "backend-keybind", "rmux"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
        custom(
            &["input", "backend-keybind", "tmux"],
            "keys",
            "KEYBINDS",
            SettingEditor::Keys,
        ),
    ]
}

fn custom_runtime_specs() -> [SettingSpec; 4] {
    [
        custom(
            &["session", "env"],
            "shell",
            "ENVIRONMENT",
            SettingEditor::Shell,
        ),
        custom(
            &["session", "max-scrollback"],
            "shell",
            "SCROLLBACK",
            SettingEditor::Shell,
        ),
        custom(
            &["window", "fullscreen-top-offset"],
            "window",
            "FULLSCREEN NOTCH",
            SettingEditor::Window,
        ),
        custom(
            &["extensions", "*"],
            "extensions",
            "EXTENSIONS",
            SettingEditor::Extensions,
        ),
    ]
}
