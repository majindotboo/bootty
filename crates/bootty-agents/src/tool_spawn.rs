//! Typed child operations. Destination, account, executable and authority belong to the host.

use serde::{Deserialize, Serialize};

use crate::AgentKind;

const MAX_SPAWN_REQUEST_BYTES: usize = 68 * 1024;
const MAX_PROMPT_BYTES: usize = 64 * 1024;
const MAX_LABEL_BYTES: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolTerminalOperation {
    Read,
    Paste,
    Submit,
    Interrupt,
    Close,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolTerminalRequest {
    pub terminal: bootty_control::CommandTarget,
    pub operation: ToolTerminalOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

impl ToolTerminalRequest {
    /// # Errors
    /// Rejects unbounded or forged input before looking up host-issued child authority.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        // JSON can escape every text/handle byte to six bytes, plus the fixed envelope.
        if bytes.len() > 6 * (64 * 1024 + 8192) + 1024 {
            return Err("Terminal request exceeds its bounded JSON envelope".into());
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        request.validate()?;
        Ok(request)
    }

    /// # Errors
    /// Requires an exact Terminal; only paste accepts bounded literal text.
    pub fn validate(&self) -> Result<(), String> {
        if self.terminal.kind != bootty_control::ResourceKind::Terminal
            || self.terminal.handle.is_empty()
            || self.terminal.handle.len() > 8192
            || self.terminal.generation == 0
        {
            return Err("An exact host-issued Terminal is required".into());
        }
        if match self.operation {
            ToolTerminalOperation::Paste => {
                self.text.as_ref().is_none_or(|text| text.len() > 64 * 1024)
            }
            _ => self.text.is_some(),
        } {
            return Err("Only paste accepts literal text, bounded to 64 KiB".into());
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolChildOperation {
    Interrupt,
    Stop,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ToolChildControlRequest {
    pub id: String,
    pub generation: u64,
    pub operation: ToolChildOperation,
}

impl ToolChildControlRequest {
    /// # Errors
    /// Rejects oversized, forged or malformed requests before invoking the child owner.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 1024 {
            return Err("Child control request exceeds 1 KiB".into());
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        request.validate()?;
        Ok(request)
    }

    /// # Errors
    /// Requires a bounded ID and a nonzero generation; the owner checks provenance.
    pub fn validate(&self) -> Result<(), String> {
        bounded_label(&self.id)?;
        if self.generation == 0 {
            return Err("Child control requires an exact live generation".into());
        }
        Ok(())
    }

    #[must_use]
    pub fn target(&self) -> bootty_control::CommandTarget {
        bootty_control::CommandTarget {
            kind: bootty_control::ResourceKind::Session,
            handle: self.id.clone(),
            generation: self.generation,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ToolSpawnRequest {
    Shell {
        name: String,
        #[serde(default)]
        title: Option<String>,
    },
    Agent {
        name: String,
        #[serde(default)]
        title: Option<String>,
        provider: AgentKind,
        #[serde(default)]
        profile: Option<String>,
        prompt: String,
    },
}

impl ToolSpawnRequest {
    /// # Errors
    /// Rejects oversized or malformed inputs before invoking any creation owner.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > MAX_SPAWN_REQUEST_BYTES {
            return Err("Typed spawn request exceeds 68 KiB".to_owned());
        }
        let request: Self = serde_json::from_slice(bytes).map_err(|error| error.to_string())?;
        request.validate()?;
        Ok(request)
    }

    /// # Errors
    /// Rejects invalid values even for a host-constructed request.
    pub fn validate(&self) -> Result<(), String> {
        let (name, title) = match self {
            Self::Shell { name, title } | Self::Agent { name, title, .. } => (name, title),
        };
        // Backend-specific name rules and collisions remain the shared mux creation owner's job.
        bounded_label(name)?;
        if let Some(title) = title {
            bounded_label(title)?;
        }
        if let Self::Agent {
            profile, prompt, ..
        } = self
        {
            if let Some(profile) = profile {
                bounded_label(profile)?;
            }
            if prompt.trim().is_empty() || prompt.len() > MAX_PROMPT_BYTES || prompt.contains('\0')
            {
                return Err(
                    "Agent prompt must be nonempty and at most 64 KiB without NUL".to_owned(),
                );
            }
        }
        Ok(())
    }

    /// The first supported agent increment reuses the exact parent's provider and profile.
    /// Upgrade only when the host can issue a separate bounded provider/account allowlist.
    /// # Errors
    /// Reports unsupported provider/account changes before creating a terminal or saved task.
    pub fn validate_parent(
        &self,
        parent_provider: AgentKind,
        parent_profile: Option<&str>,
    ) -> Result<(), String> {
        if let Self::Agent {
            provider, profile, ..
        } = self
            && (*provider != parent_provider
                || profile
                    .as_deref()
                    .is_some_and(|profile| Some(profile) != parent_profile))
        {
            return Err(
                "Spawned agents must reuse the parent's captured provider and account profile"
                    .to_owned(),
            );
        }
        Ok(())
    }
}

fn bounded_label(value: &str) -> Result<(), String> {
    if value.trim().is_empty()
        || value.len() > MAX_LABEL_BYTES
        || value.chars().any(char::is_control)
    {
        return Err(
            "Spawn names, titles and profile IDs must be 1–256 bytes without control characters"
                .to_owned(),
        );
    }
    Ok(())
}
