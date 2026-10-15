//! Speech uses the Codex sign-in already on this machine: its `ChatGPT` token, or the API key Codex
//! was configured with. Nothing else is read, and credentials never leave the helper.
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::Deserialize;
use std::{
    io::Read as _,
    path::{Path, PathBuf},
};

pub(super) struct Account {
    /// The Codex home the composer captured; `None` uses `CODEX_HOME` or `~/.codex`.
    pub directory: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Auth {
    pub token: String,
    /// The `ChatGPT` account a subscription token belongs to; absent for an API key.
    pub account_id: Option<String>,
}

#[derive(Deserialize, Default)]
struct Store {
    #[serde(rename = "OPENAI_API_KEY")]
    api_key: Option<String>,
    tokens: Option<Tokens>,
}

#[derive(Deserialize)]
struct Tokens {
    access_token: String,
    account_id: Option<String>,
}

pub(super) const SIGN_IN: &str = "Sign in to Codex to use dictation";

impl Account {
    pub fn authenticate(&self) -> Result<Auth, String> {
        codex_auth(self.directory.as_deref().map(Path::new), unix_now())
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| i64::try_from(elapsed.as_secs()).ok())
        .unwrap_or_default()
}

/// Whether the Codex sign-in in `directory` (or the default Codex home) can dictate.
pub(super) fn signed_in(directory: Option<&Path>) -> bool {
    codex_auth(directory, unix_now()).is_ok()
}

fn codex_home(directory: Option<&Path>) -> Option<PathBuf> {
    directory.map(Path::to_path_buf).or_else(|| {
        std::env::var_os("CODEX_HOME")
            .filter(|home| !home.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                std::env::var_os("HOME")
                    .or_else(|| std::env::var_os("USERPROFILE"))
                    .map(|home| PathBuf::from(home).join(".codex"))
            })
    })
}

/// A subscription token wins over an API key; an expired token is refused rather than sent.
fn codex_auth(directory: Option<&Path>, now: i64) -> Result<Auth, String> {
    let home = codex_home(directory).ok_or(SIGN_IN)?;
    let mut bytes = Vec::new();
    std::fs::File::open(home.join("auth.json"))
        .map_err(|_| SIGN_IN)?
        .take(65_537)
        .read_to_end(&mut bytes)
        .map_err(|_| "Could not read the Codex sign-in")?;
    if bytes.len() > 65_536 {
        return Err("The Codex sign-in exceeds its size limit".into());
    }
    let store: Store =
        serde_json::from_slice(&bytes).map_err(|_| "Could not read the Codex sign-in")?;
    let auth = if let Some(tokens) = store
        .tokens
        .filter(|tokens| !tokens.access_token.is_empty())
    {
        if expires_at(&tokens.access_token).is_some_and(|expiry| expiry <= now) {
            return Err("The Codex sign-in expired. Open Codex to sign in again.".into());
        }
        Auth {
            token: tokens.access_token,
            account_id: tokens.account_id.filter(|id| !id.is_empty()),
        }
    } else {
        Auth {
            token: store.api_key.filter(|key| !key.is_empty()).ok_or(SIGN_IN)?,
            account_id: None,
        }
    };
    if auth.token.len() > 8192
        || auth.token.chars().any(char::is_control)
        || auth
            .account_id
            .as_ref()
            .is_some_and(|id| id.len() > 256 || id.chars().any(char::is_control))
    {
        return Err("The Codex sign-in is invalid".into());
    }
    Ok(auth)
}

/// A JWT's `exp`, in Unix seconds, when the token is one.
fn expires_at(token: &str) -> Option<i64> {
    let payload = URL_SAFE_NO_PAD.decode(token.split('.').nth(1)?).ok()?;
    serde_json::from_slice::<serde_json::Value>(&payload)
        .ok()?
        .get("exp")?
        .as_i64()
}
