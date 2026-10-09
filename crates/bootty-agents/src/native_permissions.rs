use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::AgentKind;

/// A policy edit either leaves the current turn running or prepares a safe resume.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativePermissionUpdate {
    Queued,
    Stopped,
}

/// Explicit user choices; reusable grants must come from the captured provider request.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeApprovalDecision {
    Deny,
    AllowOnce,
    AllowSession,
    AlwaysAllow,
}

impl NativeApprovalDecision {
    pub const ALL: [Self; 4] = [
        Self::Deny,
        Self::AllowOnce,
        Self::AllowSession,
        Self::AlwaysAllow,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::AllowOnce => "allow",
            Self::AllowSession => "allow-session",
            Self::AlwaysAllow => "always-allow",
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Deny => "Deny",
            Self::AllowOnce => "Allow once",
            Self::AllowSession => "Allow for this session",
            Self::AlwaysAllow => "Always allow",
        }
    }
}

/// Provider policy retained with the conversation, independently of Bootty tool grants.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum NativePermissionMode {
    #[default]
    ProviderDefault,
    Supervised,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}

impl NativePermissionMode {
    pub const ALL: [Self; 5] = [
        Self::ProviderDefault,
        Self::Supervised,
        Self::AutoAcceptEdits,
        Self::Auto,
        Self::FullAccess,
    ];

    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::ProviderDefault => "provider-default",
            Self::Supervised => "supervised",
            Self::AutoAcceptEdits => "auto-accept-edits",
            Self::Auto => "auto",
            Self::FullAccess => "full-access",
        }
    }

    #[must_use]
    pub fn from_choice(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.label() == value || mode.id() == value)
    }

    #[must_use]
    pub const fn supports(self, provider: AgentKind) -> bool {
        !matches!((self, provider), (Self::Auto, AgentKind::Pi))
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ProviderDefault => "Provider default",
            Self::Supervised => "Supervised",
            Self::AutoAcceptEdits => "Auto-accept edits",
            Self::Auto => "Auto",
            Self::FullAccess => "Full access",
        }
    }

    #[must_use]
    pub const fn icon(self) -> &'static str {
        match self {
            Self::ProviderDefault | Self::Supervised => "lock",
            Self::AutoAcceptEdits => "pencil",
            Self::Auto => "sparkles",
            Self::FullAccess => "lock-open",
        }
    }

    pub(crate) const fn claude(self) -> Option<&'static str> {
        match self {
            Self::ProviderDefault => None,
            Self::Supervised => Some("default"),
            Self::AutoAcceptEdits => Some("acceptEdits"),
            Self::Auto => Some("auto"),
            Self::FullAccess => Some("bypassPermissions"),
        }
    }

    pub(crate) fn codex(self) -> Value {
        let (approval, reviewer, sandbox) = match self {
            Self::ProviderDefault => return json!({}),
            Self::Supervised => ("untrusted", "user", "readOnly"),
            Self::AutoAcceptEdits => ("on-request", "user", "workspaceWrite"),
            Self::Auto => ("on-request", "auto_review", "workspaceWrite"),
            Self::FullAccess => ("never", "user", "dangerFullAccess"),
        };
        json!({"approvalPolicy":approval,"approvalsReviewer":reviewer,"sandboxPolicy":{"type":sandbox}})
    }

    pub(crate) fn pi_extension(
        self,
        provider: AgentKind,
        tools: Option<&crate::ToolBridge>,
    ) -> Result<Option<tempfile::NamedTempFile>, String> {
        use std::io::Write as _;
        if provider != AgentKind::Pi || self == Self::ProviderDefault {
            return Ok(None);
        }
        if !self.supports(provider) {
            return Err("Pi does not support automatic approval review".into());
        }
        let mut file = tempfile::Builder::new()
            .prefix("bootty-pi-permissions-")
            .suffix(".mjs")
            .tempfile()
            .map_err(|e| e.to_string())?;
        let mode = serde_json::to_string(&self).map_err(|e| e.to_string())?;
        let source = include_str!("assets/pi-native-permissions.mjs")
            .replace("__BOOTTY_PERMISSION_MODE__", &mode)
            .replace(
                "__BOOTTY_PERMISSION_TOOLS__",
                &tools.map_or_else(
                    || Ok("{}".to_owned()),
                    crate::ToolBridge::pi_permission_tools,
                )?,
            );
        file.write_all(source.as_bytes())
            .map_err(|e| e.to_string())?;
        Ok(Some(file))
    }
}

impl std::str::FromStr for NativePermissionMode {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::ALL
            .into_iter()
            .find(|mode| mode.id() == value)
            .ok_or_else(|| "Unknown provider permission mode".into())
    }
}
