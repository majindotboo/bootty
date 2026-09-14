use super::model::{
    AppearanceMode, CursorStyleConfig, ExtensionSettingValue, KeybindPreset,
    MacosOptionAsAltConfig, MacosTitlebarStyle, MultiplexerBackendConfig, OnLastWindowClosed,
    OpenBehavior, PanelTabStyle, PanelTabs, RestoreOnStartup, SidebarPosition, SshProfileConfig,
    StatusSegment, WhenClosingWithNoTabs, WindowDecoration, WindowFullscreen,
};
use crate::color::Color;
use serde::{Deserialize, Deserializer};
use std::{collections::BTreeMap, path::PathBuf};
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct RawConfig {
    pub(super) locale: Option<String>,
    pub(super) version: Option<u32>,
    #[serde(rename = "restore_on_startup")]
    pub(super) restore_on_startup: Option<RestoreOnStartup>,
    #[serde(rename = "cli_default_open_behavior")]
    pub(super) cli_default_open_behavior: Option<OpenBehavior>,
    #[serde(rename = "default_open_behavior")]
    pub(super) default_open_behavior: Option<OpenBehavior>,
    #[serde(rename = "when_closing_with_no_tabs")]
    pub(super) when_closing_with_no_tabs: Option<WhenClosingWithNoTabs>,
    #[serde(rename = "on_last_window_closed")]
    pub(super) on_last_window_closed: Option<OnLastWindowClosed>,
    #[serde(rename = "auto_update")]
    pub(super) _auto_update: Option<bool>,
    pub(super) theme: Option<String>,
    pub(super) colors: ColorPatch,
    pub(super) appearance: AppearancePatch,
    pub(super) cursor: CursorPatch,
    pub(super) font: FontPatch,
    #[serde(deserialize_with = "deserialize_panels")]
    pub(super) panels: BTreeMap<super::model::PanelKind, super::model::PanelConfig>,
    pub(super) font_feature: Vec<String>,
    pub(super) chrome: ChromePatch,
    pub(super) sidebar: SidebarPatch,
    pub(super) multiplexer: MultiplexerPatch,
    pub(super) ssh_profiles: BTreeMap<String, SshProfileConfig>,
    pub(super) extensions: BTreeMap<String, BTreeMap<String, ExtensionSettingValue>>,
    pub(super) input: InputPatch,
    pub(super) session: SessionPatch,
    pub(super) diagnostics: DiagnosticsPatch,
    pub(super) window: WindowPatch,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct WindowPatch {
    pub(super) background_opacity: Option<f32>,
    pub(super) background_image: Option<PathBuf>,
    pub(super) background_image_opacity: Option<f32>,
    pub(super) background_gradient_start: Option<Color>,
    pub(super) background_gradient_end: Option<Color>,
    pub(super) background_gradient_angle: Option<f32>,
    pub(super) background_material: Option<super::model::BackgroundMaterial>,
    pub(super) title: Option<String>,
    pub(super) width: Option<f32>,
    pub(super) height: Option<f32>,
    pub(super) fullscreen_enabled: Option<bool>,
    pub(super) fullscreen: Option<WindowFullscreen>,
    pub(super) fullscreen_top_offset: Option<f32>,
    pub(super) fullscreen_tabs_in_notch: Option<bool>,
    pub(super) window_decoration: Option<WindowDecoration>,
    pub(super) macos_titlebar_style: Option<MacosTitlebarStyle>,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct FontPatch {
    pub(super) family: Option<Vec<String>>,
    pub(super) style_bold: Option<crate::FontStyleAssignment>,
    pub(super) style_italic: Option<crate::FontStyleAssignment>,
    pub(super) style_bold_italic: Option<crate::FontStyleAssignment>,
    pub(super) ui_family: Option<Vec<String>>,
    pub(super) ui_weights: Option<crate::FontWeightAssignments>,
    pub(super) ui_size: Option<f32>,
    pub(super) ui_use_terminal_family: Option<bool>,
    pub(super) features: Option<Vec<String>>,
    pub(super) size: Option<f32>,
    pub(super) cell_width: Option<f32>,
    pub(super) cell_height: Option<f32>,
    pub(super) fit_cell_height: Option<bool>,
    pub(super) fit_cell_width: Option<bool>,
    pub(super) baseline_adjustment: Option<f32>,
    pub(super) underline_position: Option<f32>,
    pub(super) underline_thickness: Option<f32>,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct TabPatch {
    pub(super) appearance: Option<super::model::TabAppearance>,
    pub(super) close_position: Option<super::model::TabClosePosition>,
    pub(super) close_button: Option<super::model::TabCloseButton>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct ChromePatch {
    pub(super) left_dock_toggle: Option<bool>,
    pub(super) right_dock_toggle: Option<bool>,
    pub(super) panel_tab_style: Option<PanelTabStyle>,
    pub(super) panel_tabs: Option<PanelTabs>,
    pub(super) dock_tabs: Option<TabPatch>,
    pub(super) terminal_tabs: Option<TabPatch>,
    pub(super) sidebar: Option<bool>,
    #[serde(alias = "status-bar")]
    pub(super) top_bar: Option<bool>,
    pub(super) bottom_bar: Option<bool>,
    #[serde(rename = "window-tabs")]
    pub(super) _window_tabs: Option<bool>,
    pub(super) sidebar_width: Option<f32>,
    pub(super) status_height: Option<f32>,
    pub(super) status_background: Option<Color>,
    pub(super) gap: Option<f32>,
    pub(super) pane_divider_width: Option<f32>,
    pub(super) pane_divider_color: Option<Color>,
    pub(super) notched_fullscreen_black_chrome: Option<bool>,
    pub(super) pane_focus_border_width: Option<f32>,
    pub(super) pane_focus_border_color: Option<Color>,
    pub(super) pane_corner_radius: Option<f32>,
    pub(super) unfocused_sidebar_dim: Option<f32>,
    pub(super) unfocused_terminal_dim: Option<f32>,
    #[serde(alias = "status-segment")]
    pub(super) top_segment: Option<Vec<StatusSegment>>,
    pub(super) bottom_segment: Option<Vec<StatusSegment>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct SidebarPatch {
    pub(super) position: Option<SidebarPosition>,
    pub(super) background: Option<Color>,
    #[serde(rename = "fullscreen-background")]
    pub(super) _fullscreen_background: Option<Color>,
    pub(super) foreground: Option<Color>,
    pub(super) selected: Option<Color>,
    pub(super) hover: Option<Color>,
    #[serde(rename = "fullscreen-hover")]
    pub(super) _fullscreen_hover: Option<Color>,
    pub(super) border: Option<Color>,
    pub(super) session_modules: Option<Vec<String>>,
    pub(super) modules: Option<Vec<String>>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct MultiplexerPatch {
    pub(super) backend: Option<MultiplexerBackendConfig>,
    // Older builds exposed this unused setting. Herdr attaches the selected backend session.
    #[serde(rename = "herdr-session")]
    pub(super) _herdr_session: Option<String>,
    pub(super) hide_tmux_status: Option<bool>,
    pub(super) remote: Option<super::RemoteConfig>,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct InputPatch {
    pub(super) modifier_remap: Option<Vec<String>>,
    pub(super) macos_option_as_alt: Option<MacosOptionAsAltConfig>,
    pub(super) hide_mouse_pointer_while_typing: Option<bool>,
    pub(super) copy_on_select: Option<bool>,
    pub(super) preset: Option<KeybindPreset>,
    pub(super) prefix: Option<String>,
    pub(super) keybind: Option<Vec<String>>,
    pub(super) sidebar_keybind: Option<Vec<String>>,
    pub(super) backend_keybind: Option<BackendKeybindPatch>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct BackendKeybindPatch {
    pub(super) herdr: Option<Vec<String>>,
    pub(super) native: Option<Vec<String>>,
    pub(super) rmux: Option<Vec<String>>,
    pub(super) tmux: Option<Vec<String>>,
}
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct SessionPatch {
    pub(super) output_archives: Option<bool>,
    pub(super) clipboard_write_hosts: Option<String>,
    pub(super) bell: Option<super::model::BellMode>,
    pub(super) command_notifications: Option<super::model::NotificationPolicy>,
    pub(super) agent_notifications: Option<super::model::NotificationPolicy>,
    pub(super) command_notification_min_seconds: Option<u32>,
    pub(super) shell_integration: Option<bool>,
    pub(super) shell: Option<String>,
    pub(super) working_directory: Option<PathBuf>,
    pub(super) env: Option<Vec<EnvConfigEntry>>,
    pub(super) term: Option<String>,
    pub(super) colorterm: Option<String>,
    pub(super) max_scrollback: Option<usize>,
    pub(super) scrollbar: Option<super::model::TerminalScrollbar>,
    pub(super) glyph_protocol: Option<bool>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct EnvConfigEntry {
    pub(super) name: String,
    pub(super) value: String,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct DiagnosticsPatch {
    pub(super) stability_trace: Option<PathBuf>,
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct ColorPatch {
    pub(super) background: Option<Color>,
    pub(super) foreground: Option<Color>,
    pub(super) cursor: Option<Color>,
    pub(super) cursor_text: Option<Color>,
    pub(super) pointer_foreground: Option<Color>,
    pub(super) pointer_background: Option<Color>,
    pub(super) tektronix_foreground: Option<Color>,
    pub(super) tektronix_background: Option<Color>,
    pub(super) highlight_background: Option<Color>,
    pub(super) tektronix_cursor: Option<Color>,
    pub(super) highlight_foreground: Option<Color>,
    pub(super) selection_background: Option<Color>,
    pub(super) selection_foreground: Option<Color>,
    pub(super) palette: Option<Vec<Color>>,
    pub(super) palette_generate: Option<bool>,
    pub(super) palette_harmonious: Option<bool>,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct AppearancePatch {
    pub(super) mode: Option<AppearanceMode>,
    #[serde(default)]
    pub(super) light: AppearanceBranchPatch,
    #[serde(default)]
    pub(super) dark: AppearanceBranchPatch,
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct AppearanceBranchPatch {
    pub(super) theme: Option<String>,
    #[serde(default)]
    pub(super) colors: ColorPatch,
}

#[derive(Clone, Copy, Debug, Default, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub(super) struct CursorPatch {
    pub(super) style: Option<CursorStyleConfig>,
    pub(super) blink: Option<bool>,
    pub(super) dim_inactive_pane: Option<bool>,
}
#[derive(Deserialize)]
#[serde(untagged)]
enum BoolOrString {
    Bool(bool),
    String(String),
}

impl<'de> Deserialize<'de> for WindowFullscreen {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = BoolOrString::deserialize(deserializer)?;
        match value {
            BoolOrString::Bool(false) => Ok(Self::Disabled),
            BoolOrString::Bool(true) => Ok(Self::Native),
            BoolOrString::String(value) => parse_window_fullscreen(&value)
                .ok_or_else(|| serde::de::Error::custom(format!("invalid fullscreen: {value}"))),
        }
    }
}

impl<'de> Deserialize<'de> for WindowDecoration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = BoolOrString::deserialize(deserializer)?;
        match value {
            BoolOrString::Bool(false) => Ok(Self::None),
            BoolOrString::Bool(true) => Ok(Self::Auto),
            BoolOrString::String(value) => parse_window_decoration(&value).ok_or_else(|| {
                serde::de::Error::custom(format!("invalid window-decoration: {value}"))
            }),
        }
    }
}

impl<'de> Deserialize<'de> for MacosTitlebarStyle {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        parse_macos_titlebar_style(&value).ok_or_else(|| {
            serde::de::Error::custom(format!("invalid macos-titlebar-style: {value}"))
        })
    }
}

fn parse_window_fullscreen(input: &str) -> Option<WindowFullscreen> {
    match normalize_config_value(input).as_str() {
        "false" | "off" | "disabled" | "none" | "no" => Some(WindowFullscreen::Disabled),
        "true" | "native" | "yes" => Some(WindowFullscreen::Native),
        "non_native" => Some(WindowFullscreen::NonNative),
        "non_native_visible_menu" => Some(WindowFullscreen::NonNativeVisibleMenu),
        "non_native_padded_notch" => Some(WindowFullscreen::NonNativePaddedNotch),
        _ => None,
    }
}

fn parse_window_decoration(input: &str) -> Option<WindowDecoration> {
    match normalize_config_value(input).as_str() {
        "false" | "none" | "off" | "disabled" | "no" => Some(WindowDecoration::None),
        "true" | "auto" | "on" | "yes" => Some(WindowDecoration::Auto),
        "client" => Some(WindowDecoration::Client),
        "server" => Some(WindowDecoration::Server),
        _ => None,
    }
}

fn parse_macos_titlebar_style(input: &str) -> Option<MacosTitlebarStyle> {
    match normalize_config_value(input).as_str() {
        "native" => Some(MacosTitlebarStyle::Native),
        "transparent" => Some(MacosTitlebarStyle::Transparent),
        "hidden" => Some(MacosTitlebarStyle::Hidden),
        _ => None,
    }
}

fn normalize_config_value(input: &str) -> String {
    input.trim().to_ascii_lowercase().replace('-', "_")
}

fn deserialize_panels<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<super::model::PanelKind, super::model::PanelConfig>, D::Error> {
    use serde::de::IntoDeserializer as _;
    let panels = BTreeMap::<String, super::model::PanelConfig>::deserialize(deserializer)?;
    panels
        .into_iter()
        .filter(|(name, _)| !matches!(name.as_str(), "jobs" | "transfers" | "recovery" | "shell"))
        .map(|(name, value)| {
            let kind = super::model::PanelKind::deserialize(name.into_deserializer())?;
            Ok((kind, value))
        })
        .collect()
}
