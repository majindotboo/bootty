use super::model::{
    AppearanceBranchConfig, AppearanceConfig, AppearanceMode, BackendKeybindConfig, BoottyConfig,
    ChromeConfig, CursorConfig, DiagnosticsConfig, FontConfig, InputConfig, KeybindPreset,
    MacosOptionAsAltConfig, MacosTitlebarStyle, MultiplexerConfig, OnLastWindowClosed,
    OpenBehavior, RestoreOnStartup, SegmentAlign, SessionConfig, SidebarConfig, SidebarPosition,
    StatusSegment, WhenClosingWithNoTabs, WindowConfig, WindowDecoration, WindowFullscreen,
};
use super::theme_catalog::{
    DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, default_dark_colors, default_light_colors,
};
use std::{
    collections::BTreeMap,
    env,
    ffi::OsString,
    path::{Path, PathBuf},
};
const DEFAULT_MAX_SCROLLBACK: usize = 320_000_000;
const DEFAULT_TERM: &str = "xterm-bootty";
const DEFAULT_FONT_FAMILY: &str = ".ZedMono";
const DEFAULT_UI_FONT_FAMILY: &str = ".ZedSans";
const DEFAULT_FONT_SIZE: f32 = 15.0;
const DEFAULT_UI_FONT_SIZE: f32 = 16.0;
const DEFAULT_FONT_FIT_CELL_HEIGHT: bool = true;
const DEFAULT_FONT_FIT_CELL_WIDTH: bool = false;
const DEFAULT_FONT_BASELINE_ADJUSTMENT: f32 = 0.0;
const DEFAULT_FONT_UNDERLINE_POSITION: f32 = 2.0;
const DEFAULT_FONT_UNDERLINE_THICKNESS: f32 = 1.0;
impl Default for SidebarConfig {
    fn default() -> Self {
        Self {
            position: SidebarPosition::Left,
            background: None,
            foreground: None,
            selected: None,
            hover: None,
            border: None,
            session_modules: vec![
                "diffs".to_owned(),
                "process".to_owned(),
                "directory".to_owned(),
                "branch".to_owned(),
                "ports".to_owned(),
                "progress".to_owned(),
            ],
            session_modules_configured: false,
            modules: vec!["sessions".to_owned(), "codexbar".to_owned()],
        }
    }
}
fn default_status_segments() -> Vec<StatusSegment> {
    vec![
        StatusSegment {
            align: SegmentAlign::Left,
            module: "session".to_owned(),
            ..StatusSegment::default()
        },
        StatusSegment {
            align: SegmentAlign::Left,
            module: "windows".to_owned(),
            ..StatusSegment::default()
        },
        StatusSegment {
            align: SegmentAlign::Right,
            module: "sysinfo".to_owned(),
            ..StatusSegment::default()
        },
        StatusSegment {
            align: SegmentAlign::Right,
            module: "clock".to_owned(),
            ..StatusSegment::default()
        },
    ]
}
impl Default for FontConfig {
    fn default() -> Self {
        Self {
            family: vec![DEFAULT_FONT_FAMILY.to_owned()],
            style_bold: crate::FontStyleAssignment::Automatic,
            style_italic: crate::FontStyleAssignment::Automatic,
            style_bold_italic: crate::FontStyleAssignment::Automatic,
            ui_family: vec![DEFAULT_UI_FONT_FAMILY.to_owned()],
            ui_weights: crate::FontWeightAssignments::new(),
            ui_size: DEFAULT_UI_FONT_SIZE,
            ui_use_terminal_family: false,
            features: Vec::new(),
            size: DEFAULT_FONT_SIZE,
            cell_width: None,
            cell_height: None,
            fit_cell_height: DEFAULT_FONT_FIT_CELL_HEIGHT,
            fit_cell_width: DEFAULT_FONT_FIT_CELL_WIDTH,
            baseline_adjustment: DEFAULT_FONT_BASELINE_ADJUSTMENT,
            underline_position: DEFAULT_FONT_UNDERLINE_POSITION,
            underline_thickness: DEFAULT_FONT_UNDERLINE_THICKNESS,
        }
    }
}

impl Default for ChromeConfig {
    fn default() -> Self {
        Self {
            left_dock_toggle: true,
            right_dock_toggle: true,
            panel_tab_style: super::model::PanelTabStyle::default(),
            panel_tabs: super::model::PanelTabs::default(),
            dock_tabs: super::model::TabConfig {
                appearance: super::model::TabAppearance::Segmented,
                ..Default::default()
            },
            terminal_tabs: super::model::TabConfig {
                close_button: super::model::TabCloseButton::Always,
                ..Default::default()
            },
            sidebar: true,
            top_bar: true,
            bottom_bar: false,
            status_background: None,
            sidebar_width: 286.0,
            status_height: 30.0,
            gap: 0.0,
            pane_divider_width: 1.0,
            pane_divider_color: None,
            notched_fullscreen_black_chrome: true,
            pane_focus_border_width: 0.0,
            pane_focus_border_color: None,
            pane_corner_radius: 0.0,
            unfocused_sidebar_dim: 0.16,
            unfocused_terminal_dim: 0.0,
            top_segments: default_status_segments(),
            bottom_segments: Vec::new(),
        }
    }
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            output_archives: false,
            clipboard_write_hosts: String::new(),
            bell: super::model::BellMode::default(),
            command_notifications: super::model::NotificationPolicy::default(),
            agent_notifications: super::model::NotificationPolicy::default(),
            command_notification_min_seconds: 10,
            shell_integration: false,
            shell: None,
            working_directory: None,
            env: Vec::new(),
            term: DEFAULT_TERM.to_owned(),
            colorterm: "truecolor".to_owned(),
            max_scrollback: DEFAULT_MAX_SCROLLBACK,
            scrollbar: super::model::TerminalScrollbar::default(),
            glyph_protocol: true,
        }
    }
}

#[must_use]
pub fn default_working_directory() -> Option<PathBuf> {
    default_working_directory_from(|name| env::var_os(name))
}

fn default_working_directory_from(
    mut var: impl FnMut(&str) -> Option<OsString>,
) -> Option<PathBuf> {
    #[cfg(windows)]
    {
        if let Some(user_profile) = non_empty_env_path(var("USERPROFILE")) {
            return Some(user_profile);
        }
        let home_drive = non_empty_env_path(var("HOMEDRIVE"))?;
        let home_path = non_empty_env_path(var("HOMEPATH"))?;
        Some(home_drive.join(home_path))
    }

    #[cfg(not(windows))]
    {
        non_empty_env_path(var("HOME"))
    }
}

fn non_empty_env_path(value: Option<OsString>) -> Option<PathBuf> {
    value.filter(|value| !value.is_empty()).map(PathBuf::from)
}

impl Default for AppearanceConfig {
    fn default() -> Self {
        let light = default_light_colors();
        let dark = default_dark_colors();
        Self {
            mode: AppearanceMode::System,
            light: AppearanceBranchConfig {
                theme: Some(DEFAULT_LIGHT_THEME.to_owned()),
                theme_colors: light.clone(),
                colors: light,
            },
            dark: AppearanceBranchConfig {
                theme: Some(DEFAULT_DARK_THEME.to_owned()),
                theme_colors: dark.clone(),
                colors: dark,
            },
        }
    }
}

impl Default for InputConfig {
    fn default() -> Self {
        let mut input = Self {
            modifier_remap: Vec::new(),
            macos_option_as_alt: MacosOptionAsAltConfig::default(),
            hide_mouse_pointer_while_typing: true,
            copy_on_select: false,
            preset: KeybindPreset::default(),
            prefix: None,
            keybind: Vec::new(),
            sidebar_keybind: Vec::new(),
            backend_keybinds: BackendKeybindConfig::default(),
        };
        input.reset_default_keybinds();
        input
    }
}

impl Default for BoottyConfig {
    fn default() -> Self {
        Self {
            version: 1,
            locale: "en".to_owned(),
            restore_on_startup: RestoreOnStartup::default(),
            cli_default_open_behavior: OpenBehavior::default(),
            default_open_behavior: OpenBehavior::default(),
            when_closing_with_no_tabs: WhenClosingWithNoTabs::default(),
            on_last_window_closed: OnLastWindowClosed::default(),
            appearance: AppearanceConfig::default(),
            cursor: CursorConfig::default(),
            font: FontConfig::default(),
            chrome: ChromeConfig::default(),
            panels: BTreeMap::new(),
            sidebar: SidebarConfig::default(),
            multiplexer: MultiplexerConfig::default(),
            ssh_profiles: BTreeMap::new(),
            extensions: BTreeMap::new(),
            input: InputConfig::default(),
            session: SessionConfig::default(),
            diagnostics: DiagnosticsConfig::default(),
            window: WindowConfig {
                background_opacity: 1.0,
                background_image: None,
                background_image_opacity: 1.0,
                background_gradient_start: None,
                background_gradient_end: None,
                background_gradient_angle: 180.0,
                background_material: super::model::BackgroundMaterial::Opaque,
                title: "Bootty".to_owned(),
                width: 1220.0,
                height: 760.0,
                fullscreen_enabled: false,
                fullscreen: WindowFullscreen::default(),
                fullscreen_top_offset: None,
                fullscreen_tabs_in_notch: true,
                window_decoration: WindowDecoration::default(),
                macos_titlebar_style: MacosTitlebarStyle::default(),
            },
            config_path: default_config_path(),
            compatibility_warnings: Vec::new(),
        }
    }
}

#[must_use]
pub fn default_config_path() -> PathBuf {
    config_path_from_env(
        std::env::var_os("XDG_CONFIG_HOME"),
        std::env::var_os("HOME"),
    )
}

pub fn config_path_from_env(
    xdg_config_home: Option<impl AsRef<Path>>,
    home: Option<impl AsRef<Path>>,
) -> PathBuf {
    crate::identity::config_path_from_env(
        crate::identity::ApplicationIdentity::Production,
        xdg_config_home,
        home,
    )
}
