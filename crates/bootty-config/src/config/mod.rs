mod defaults;
mod keybind_presets;
mod load;
mod model;
mod raw;
mod remote;
pub use remote::{RemoteConfig, WslDistribution, WslRemoteConfig};
mod resolve;
mod theme_catalog;
pub mod theme_file;
mod writeback;

pub use defaults::{config_path_from_env, default_config_path, default_working_directory};
pub use keybind_presets::split_keybind_entry;
pub use load::{
    ConfigDocument, ConfigFileSnapshot, ConfigLoadError, ConfigResult, config_file_snapshot,
    load_config_document, load_config_from_path, load_or_create_config_document,
};
pub use model::{
    AppearanceBranchConfig, AppearanceConfig, AppearanceMode, AppearanceVariant,
    BackendKeybindConfig, BackgroundMaterial, BellMode, BoottyConfig, ChromeConfig, ColorConfig,
    CursorConfig, CursorStyleConfig, DiagnosticsConfig, ExtensionSettingValue, FontConfig,
    InputConfig, KeybindPreset, MacosOptionAsAltConfig, MacosTitlebarStyle,
    MultiplexerBackendConfig, MultiplexerConfig, MultiplexerConfigError, NotificationPolicy,
    OnLastWindowClosed, OpenBehavior, PanelButton, PanelConfig, PanelDock, PanelKind,
    PanelTabStyle, PanelTabs, ResolvedTheme, RestoreOnStartup, SegmentAlign, SessionConfig,
    SidebarConfig, SidebarPosition, SshAuthenticationConfig, SshHostKeyPolicyConfig,
    SshProfileConfig, SshRemoteConfig, StatusSegment, TabAppearance, TabCloseButton,
    TabClosePosition, TabConfig, TerminalScrollbar, ThemeInfo, WhenClosingWithNoTabs, WindowConfig,
    WindowDecoration, WindowFullscreen, config_token,
};
pub use resolve::{available_theme_names, resolve_theme};
pub use theme_catalog::{
    DEFAULT_DARK_THEME, DEFAULT_LIGHT_THEME, builtin_theme_names, parse_theme_source,
};
pub use writeback::{
    AcceptedConfigDocument, ConfigWriteOutcome, commit_config_document, update_config_document,
    write_font_size_preference,
};

pub(crate) use load::{config_dependency_snapshot, load_config_attempt};
