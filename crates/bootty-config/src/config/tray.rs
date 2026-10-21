//! Process-wide menu bar / system tray preferences.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct TrayConfig {
    pub visibility: TrayVisibility,
    pub icon_style: TrayIconStyle,
    /// macOS can display a count beside the status icon; other trays expose it in the tooltip.
    pub macos_unread_count: bool,
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, strum::IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum TrayVisibility {
    #[default]
    Hidden,
    Always,
    Working,
    Attention,
}

impl TrayVisibility {
    #[must_use]
    pub const fn allows(self, working: bool, attention: bool) -> bool {
        match self {
            Self::Hidden => false,
            Self::Always => true,
            Self::Working => working,
            Self::Attention => attention,
        }
    }
}

#[derive(
    Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, strum::IntoStaticStr,
)]
#[serde(rename_all = "kebab-case")]
#[strum(serialize_all = "kebab-case")]
pub enum TrayIconStyle {
    #[cfg_attr(not(target_os = "macos"), default)]
    Color,
    #[cfg_attr(target_os = "macos", default)]
    Monochrome,
}
