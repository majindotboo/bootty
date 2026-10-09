//! Host-neutral settings projection and interaction contract.

pub use bootty_config::settings_schema::{NumberControl, SettingValue as ScalarValue};

use crate::settings_session::FontFeatureDraft;

use super::font_features::FontFeatureEditorSnapshot;

/// A top-level settings destination in stable navigation order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub enum SettingsCategory {
    #[default]
    General,
    Appearance,
    Theme,
    Keymap,
    WindowAndLayout,
    Panels,
    Terminal,
    Browser,
    Permissions,
    Providers,
    Remotes,
    Advanced,
}

impl SettingsCategory {
    /// Stable identity used by navigation and persisted window state.
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Appearance => "appearance",
            Self::Theme => "theme",
            Self::Keymap => "keymap",
            Self::WindowAndLayout => "window-and-layout",
            Self::Panels => "panels",
            Self::Terminal => "terminal",
            Self::Browser => "browser",
            Self::Permissions => "permissions",
            Self::Providers => "providers",
            Self::Remotes => "remotes",
            Self::Advanced => "advanced",
        }
    }
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Appearance => "Appearance",
            Self::Theme => "Theme",
            Self::Keymap => "Keyboard",
            Self::WindowAndLayout => "Window & Layout",
            Self::Panels => "Panels",
            Self::Terminal => "Terminal",
            Self::Browser => "Browser",
            Self::Permissions => "Permissions",
            Self::Providers => "Providers",
            Self::Remotes => "Remotes",
            Self::Advanced => "Advanced",
        }
    }
}

/// Config-derived controls. Navigation and input focus belong to the settings view.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsContent {
    pub pages: Vec<SettingsPage>,
    pub write_error: Option<String>,
}

/// One top-level settings page in the flat order consumed by Zed's settings list.
#[derive(Clone, Debug, PartialEq)]
pub struct SettingsPage {
    pub category: SettingsCategory,
    pub title: String,
    pub search_terms: String,
    pub items: Vec<SettingsPageItem>,
}

/// One item in a settings page.
///
/// This mirrors Zed's flat `SettingsPageItem` list: section headers are both rendered content and
/// navbar anchors, while setting rows retain Bootty's typed persistence-neutral DTO.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsPageItem {
    SectionHeader {
        id: String,
        title: String,
        search_terms: String,
    },
    Setting(SettingsRow),
    /// A discriminating parent setting and the rows whose meaning depends on it.
    ///
    /// This mirrors Zed's `DynamicItem`: the host projects only the relevant children for the
    /// current configuration, and the renderer presents them as one nested setting group.
    Dependent {
        parent: SettingsRow,
        children: Vec<SettingsRow>,
    },
}

/// Provider cards double as navigation sections without adding a second visible heading.
pub(super) fn provider_kind_for_settings_row(
    row: &SettingsRow,
) -> Option<bootty_agents::AgentKind> {
    let SettingsRow::Value {
        id,
        control: SettingsControl::Toggle,
        ..
    } = row
    else {
        return None;
    };
    let provider = id.strip_prefix("agents.")?.strip_suffix(".enabled")?;
    bootty_agents::AgentKind::ALL
        .into_iter()
        .find(|kind| kind.to_string() == provider)
}

pub(super) fn provider_navigation_id(provider: bootty_agents::AgentKind) -> String {
    format!("providers:{provider}")
}

/// One settings row. Custom editors project into the same small vocabulary as schema rows.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsRow {
    Section(String),
    Notice {
        text: String,
        destructive: bool,
    },
    Value {
        id: String,
        label: String,
        help: String,
        value: ScalarValue,
        control: SettingsControl,
        enabled: bool,
    },
    AnsiPalette {
        id: String,
        label: String,
        help: String,
        colors: Vec<String>,
        presets: Vec<AnsiPalettePreset>,
    },
    Action {
        id: String,
        label: String,
        help: String,
        button: String,
        enabled: bool,
    },
    StringList {
        id: String,
        label: String,
        help: String,
        items: Vec<String>,
        options: Vec<String>,
        add_label: String,
        enabled: bool,
    },
    ModifierRemaps {
        id: String,
        label: String,
        help: String,
        mappings: Vec<ModifierRemap>,
        choices: Vec<SettingsChoice>,
        enabled: bool,
    },
    Environment {
        id: String,
        label: String,
        help: String,
        items: Vec<EnvironmentVariable>,
        enabled: bool,
    },
    FontFeatures {
        id: String,
        label: String,
        help: String,
        editor: FontFeatureEditorSnapshot,
    },
    StatusSegments(StatusSegmentsSnapshot),
    /// Native integration controls and any unsupported-source diagnostic for one provider.
    ModuleIntegrations(ModuleIntegrationsSnapshot),
    Remote(RemoteEditorSnapshot),
}

/// One complete palette replacement offered by the ANSI palette editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AnsiPalettePreset {
    pub label: String,
    pub colors: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModifierRemap {
    pub source: String,
    pub target: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModifierRemapField {
    Source,
    Target,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentVariable {
    pub name: String,
    pub value: String,
}

/// Presentation metadata for the scalar setting controls.
#[derive(Clone, Debug, PartialEq)]
pub enum SettingsControl {
    Toggle,
    Text {
        placeholder: String,
        optional: bool,
    },
    Number {
        range: std::ops::RangeInclusive<f32>,
        control: NumberControl,
        precision: usize,
        suffix: String,
        display_scale: f32,
        optional: bool,
    },
    Choice(Vec<SettingsChoice>),
    /// A searchable picker for theme names. This stays distinct from small finite choices so
    /// the control can preserve the selected theme while filtering a larger catalog.
    Theme(Vec<SettingsChoice>),
    Color,
    ReadOnly,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsChoice {
    pub token: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsListItem {
    pub id: String,
    pub label: String,
    pub detail: Option<String>,
    pub selected: bool,
}

/// One complete structured status-bar editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSegmentsSnapshot {
    pub id: String,
    pub label: String,
    pub help: String,
    pub modules: Vec<SettingsChoice>,
    pub segments: Vec<StatusSegmentEditorRow>,
    pub add_label: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StatusSegmentEditorRow {
    pub module: String,
    pub alignment: StatusSegmentAlignment,
    pub foreground: Option<String>,
    pub background: Option<String>,
    pub icon: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum StatusSegmentAlignment {
    #[default]
    Left,
    Center,
    Right,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusSegmentColor {
    Foreground,
    Background,
}

/// One interaction emitted by the structured status-segment editor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StatusSegmentIntent {
    Add {
        module: String,
    },
    Remove {
        index: usize,
    },
    Move {
        index: usize,
        offset: isize,
    },
    SetModule {
        index: usize,
        module: String,
    },
    SetAlignment {
        index: usize,
        alignment: StatusSegmentAlignment,
    },
    SetColor {
        index: usize,
        field: StatusSegmentColor,
        value: Option<String>,
    },
    SetIcon {
        index: usize,
        icon: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleIntegrationsSnapshot {
    pub identity: String,
    pub error: Option<String>,
    pub integrations: Vec<ModuleIntegrationSnapshot>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModuleIntegrationStatus {
    Missing,
    Partial,
    Installed,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModuleIntegrationSnapshot {
    pub module: String,
    pub id: String,
    pub title: String,
    pub summary: String,
    pub status: ModuleIntegrationStatus,
}

/// Host-neutral integration operation emitted by the settings presentation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModuleSourceIntent {
    InstallIntegration {
        identity: String,
        module: String,
        id: String,
    },
    UninstallIntegration {
        identity: String,
        module: String,
        id: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteEditorSnapshot {
    pub id: String,
    pub label: String,
    pub detail: String,
    pub error: Option<String>,
    pub profile: Option<RemoteProfileSnapshot>,
    pub test_state: RemoteTestState,
    pub actions: Vec<SettingsListItem>,
}

/// A remote profile projected into host-neutral fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProfileSnapshot {
    pub id: String,
    pub fields: Vec<RemoteProfileFieldSnapshot>,
    pub arguments: Vec<String>,
    pub test: Option<RemoteTestIntent>,
}

/// One opaque remote-profile field and any choices its host accepts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProfileFieldSnapshot {
    pub id: String,
    pub label: String,
    pub value: String,
    pub options: Vec<RemoteProfileOption>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteProfileOption {
    pub id: String,
    pub label: String,
}

/// Everything the host needs to identify and test the projected remote draft.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoteTestIntent {
    pub profile_id: String,
    pub fields: Vec<(String, String)>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum RemoteTestState {
    #[default]
    Idle,
    Testing,
    Passed,
    Failed(String),
}

/// A user interaction for the existing settings owner to apply.
#[derive(Clone, Debug)]
pub enum SettingsIntent {
    Close,
    Apply,
    DiscardChanges,
    SetText {
        id: String,
        value: String,
    },
    SetAnsiPaletteColor {
        id: String,
        index: usize,
        value: String,
    },
    ReplaceAnsiPalette {
        id: String,
        colors: Vec<String>,
    },
    EditStatusSegments {
        id: String,
        edit: StatusSegmentIntent,
    },
    SetRemoteField {
        profile_id: String,
        field_id: String,
        value: String,
    },
    SetValue {
        id: String,
        value: ScalarValue,
    },
    RemoveValue(String),
    Invoke(String),
    SetStringListItem {
        id: String,
        index: usize,
        value: String,
    },
    AddStringListItem(String),
    RemoveStringListItem {
        id: String,
        index: usize,
    },
    MoveStringListItem {
        id: String,
        index: usize,
        offset: isize,
    },
    SetModifierRemap {
        index: usize,
        field: ModifierRemapField,
        value: String,
    },
    AddModifierRemap,
    RemoveModifierRemap(usize),
    MoveModifierRemap {
        index: usize,
        offset: isize,
    },
    SetEnvironmentName {
        index: usize,
        value: String,
    },
    SetEnvironmentValue {
        index: usize,
        value: String,
    },
    AddEnvironmentVariable,
    RemoveEnvironmentVariable(usize),
    MoveEnvironmentVariable {
        index: usize,
        offset: isize,
    },
    ReplaceFontFeatures(Vec<FontFeatureDraft>),
    Module(ModuleSourceIntent),
    TestRemote(RemoteTestIntent),
}

/// The part of the workspace whose colors are being edited.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ThemeColorGroup {
    #[default]
    Terminal,
    Selection,
    Cursor,
    Palette,
    Window,
    Sidebar,
    Graphics,
}

impl ThemeColorGroup {
    pub const ALL: [Self; 7] = [
        Self::Terminal,
        Self::Selection,
        Self::Cursor,
        Self::Palette,
        Self::Window,
        Self::Sidebar,
        Self::Graphics,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Terminal => "Terminal",
            Self::Selection => "Selection",
            Self::Cursor => "Cursor and pointer",
            Self::Palette => "ANSI palette",
            Self::Window => "Window",
            Self::Sidebar => "Sidebars",
            Self::Graphics => "Terminal graphics",
        }
    }

    /// Classify the original setting path; presentation never rewrites persistence identity.
    #[must_use]
    pub fn for_setting(id: &str) -> Option<Self> {
        if matches!(
            id,
            "sidebar.background"
                | "sidebar.foreground"
                | "sidebar.hover"
                | "sidebar.selected"
                | "sidebar.border"
        ) {
            return Some(Self::Sidebar);
        }
        if matches!(
            id,
            "chrome.status-background"
                | "chrome.pane-divider-color"
                | "chrome.pane-focus-border-color"
        ) {
            return Some(Self::Window);
        }
        let leaf = id.rsplit('.').next()?;
        match leaf {
            "background" | "foreground" => Some(Self::Terminal),
            "selection-background"
            | "selection-foreground"
            | "highlight-background"
            | "highlight-foreground" => Some(Self::Selection),
            "cursor" | "cursor-text" | "pointer-foreground" | "pointer-background" => {
                Some(Self::Cursor)
            }
            "palette" | "palette-generate" | "palette-harmonious" => Some(Self::Palette),
            "tektronix-background" | "tektronix-foreground" | "tektronix-cursor" => {
                Some(Self::Graphics)
            }
            _ => None,
        }
    }
}

/// Project draft terminal colors without loading files or publishing application configuration.
/// Missing overrides restore the selected theme's resolved colors.
#[must_use]
pub fn theme_colors_from_document(
    document: &bootty_config::config::ConfigDocument,
    prefix: &str,
    base: &bootty_config::config::ColorConfig,
) -> bootty_config::config::ColorConfig {
    use bootty_config::color::Color;
    let mut colors = base.clone();
    for (leaf, slot) in [
        ("background", &mut colors.background),
        ("foreground", &mut colors.foreground),
        ("cursor", &mut colors.cursor),
        ("cursor-text", &mut colors.cursor_text),
        ("pointer-foreground", &mut colors.pointer_foreground),
        ("pointer-background", &mut colors.pointer_background),
        ("selection-background", &mut colors.selection_background),
        ("selection-foreground", &mut colors.selection_foreground),
        ("highlight-background", &mut colors.highlight_background),
        ("highlight-foreground", &mut colors.highlight_foreground),
        ("tektronix-background", &mut colors.tektronix_background),
        ("tektronix-foreground", &mut colors.tektronix_foreground),
        ("tektronix-cursor", &mut colors.tektronix_cursor),
    ] {
        let mut path: Vec<_> = prefix.split('.').collect();
        path.push(leaf);
        if let Some(value) = document
            .str_at(&path)
            .and_then(|value| Color::from_hex(value).ok())
        {
            *slot = Some(value);
        }
    }
    let mut path: Vec<_> = prefix.split('.').collect();
    path.push("palette");
    if let Some(palette) = document.string_array(&path) {
        colors.palette = palette
            .iter()
            .filter_map(|value| Color::from_hex(value).ok())
            .collect();
    }
    colors
}
