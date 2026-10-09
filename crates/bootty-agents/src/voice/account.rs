//! Public `OpenAI` audio uses API credentials, never a provider subscription token.
use serde::Deserialize;
use std::{io::Read as _, path::PathBuf};

pub(super) struct Account {
    pub provider: String,
    pub directory: Option<String>,
}

pub(super) struct Auth {
    pub token: String,
}

#[derive(Deserialize, Default)]
struct Store {
    #[serde(rename = "OPENAI_API_KEY")]
    codex_key: Option<String>,
    openai: Option<ApiKey>,
}
#[derive(Deserialize)]
struct ApiKey {
    #[serde(rename = "type")]
    kind: String,
    key: String,
}

impl Account {
    pub fn authenticate(&self) -> Result<Auth, String> {
        let key = std::env::var("OPENAI_API_KEY")
            .ok()
            .filter(|key| !key.is_empty());
        let key = if let Some(key) = key {
            key
        } else {
            let directory = self
                .directory
                .as_ref()
                .map(PathBuf::from)
                .or_else(|| {
                    let variable = match self.provider.as_str() {
                        "pi" => "PI_CODING_AGENT_DIR",
                        "codex" => "CODEX_HOME",
                        _ => return None,
                    };
                    std::env::var_os(variable).map(PathBuf::from).or_else(|| {
                        let home =
                            std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
                        Some(PathBuf::from(home).join(if self.provider == "pi" {
                            ".pi/agent"
                        } else {
                            ".codex"
                        }))
                    })
                })
                .ok_or("Configure an OpenAI API key to use dictation and voice")?;
            let mut bytes = Vec::new();
            std::fs::File::open(directory.join("auth.json"))
                .map_err(|_| "Configure an OpenAI API key to use dictation and voice")?
                .take(65_537)
                .read_to_end(&mut bytes)
                .map_err(|_| "Could not read the OpenAI API key")?;
            if bytes.len() > 65_536 {
                return Err("The OpenAI account store exceeds its size limit".into());
            }
            let store: Store =
                serde_json::from_slice(&bytes).map_err(|_| "Could not read the OpenAI API key")?;
            store
                .codex_key
                .or_else(|| {
                    store
                        .openai
                        .filter(|key| key.kind == "api_key")
                        .map(|key| key.key)
                })
                .ok_or("Configure an OpenAI API key to use dictation and voice")?
        };
        if key.is_empty() || key.len() > 8192 || key.chars().any(char::is_control) {
            return Err("The OpenAI API key is invalid".into());
        }
        Ok(Auth { token: key })
    }
}
