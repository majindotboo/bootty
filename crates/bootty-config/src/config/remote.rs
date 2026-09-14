use super::SshRemoteConfig;
use serde::{Deserialize, Serialize};

/// A validated WSL distribution name, passed as one argument to wsl.exe.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(try_from = "String", into = "String")]
pub struct WslDistribution(String);

impl WslDistribution {
    /// Validate a distribution name for use as one command argument.
    ///
    /// # Errors
    /// Rejects empty names, names longer than 255 bytes, leading dashes, and
    /// control characters.
    pub fn new(name: impl Into<String>) -> Result<Self, String> {
        let name = name.into();
        if name.trim().is_empty()
            || name.len() > 255
            || name.starts_with('-')
            || name.chars().any(char::is_control)
        {
            return Err("WSL distribution must be nonempty, at most 255 bytes, and contain no leading dash or control characters".to_owned());
        }
        Ok(Self(name))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl TryFrom<String> for WslDistribution {
    type Error = String;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}
impl From<WslDistribution> for String {
    fn from(value: WslDistribution) -> Self {
        value.0
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WslRemoteConfig {
    pub distribution: WslDistribution,
}

/// Legacy SSH tables retain their wire shape. A distribution selects WSL explicitly.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum RemoteConfig {
    Wsl(WslRemoteConfig),
    Ssh(SshRemoteConfig),
}
impl RemoteConfig {
    #[must_use]
    pub const fn as_ssh(&self) -> Option<&SshRemoteConfig> {
        match self {
            Self::Ssh(remote) => Some(remote),
            Self::Wsl(_) => None,
        }
    }
    #[must_use]
    pub fn host(&self) -> &str {
        match self {
            Self::Ssh(remote) => &remote.host,
            Self::Wsl(remote) => remote.distribution.as_str(),
        }
    }
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Ssh(remote) => remote.host.clone(),
            Self::Wsl(remote) => format!("WSL: {}", remote.distribution.as_str()),
        }
    }
}
impl From<SshRemoteConfig> for RemoteConfig {
    fn from(remote: SshRemoteConfig) -> Self {
        Self::Ssh(remote)
    }
}
impl From<WslRemoteConfig> for RemoteConfig {
    fn from(remote: WslRemoteConfig) -> Self {
        Self::Wsl(remote)
    }
}
