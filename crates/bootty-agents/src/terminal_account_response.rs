use serde::Deserialize;
use serde_json::Value;

use crate::{AgentKind, TerminalAccountStatus};

// Account labels are small protocol metadata. Revisit if a provider expands its schema.
const MAX_RESPONSE: usize = 64 * 1024;

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, String> {
    if bytes.len() > MAX_RESPONSE {
        return Err("Provider account response exceeds 64 KiB".to_owned());
    }
    serde_json::from_slice(bytes).map_err(|_| "Provider account response is malformed".to_owned())
}

fn label(value: Option<String>) -> Option<String> {
    value.filter(|value| {
        !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control)
    })
}

const fn status(provider: AgentKind, authenticated: Option<bool>) -> TerminalAccountStatus {
    TerminalAccountStatus {
        provider,
        authenticated,
        detail: None,
        account: None,
        auth_method: None,
        subscription: None,
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAccount {
    logged_in: bool,
    auth_method: Option<String>,
    email: Option<String>,
    org_name: Option<String>,
    subscription_type: Option<String>,
}

pub fn claude(bytes: &[u8], successful: bool) -> Result<TerminalAccountStatus, String> {
    let response: ClaudeAccount = decode(bytes)?;
    if response.logged_in != successful {
        return Err("Claude account check failed".to_owned());
    }
    let mut result = status(AgentKind::Claude, Some(response.logged_in));
    result.auth_method = label(response.auth_method);
    if response.logged_in {
        result.account = label(response.email).or_else(|| label(response.org_name));
        if matches!(
            result.auth_method.as_deref(),
            Some("claude.ai" | "oauth_token")
        ) {
            result.subscription = label(response.subscription_type);
        }
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PiAccount {
    status: String,
    provider: String,
    auth_type: Option<String>,
    reason: Option<String>,
}

pub fn pi(bytes: &[u8], successful: bool, provider: &str) -> Result<TerminalAccountStatus, String> {
    let response: PiAccount = decode(bytes)?;
    if !response.provider.eq_ignore_ascii_case(provider) {
        return Err("Pi account check reported a different model provider".to_owned());
    }
    match (response.status.as_str(), successful) {
        ("ready", true) => {
            let mut result = status(AgentKind::Pi, Some(true));
            result.auth_method = label(response.auth_type);
            // Pi's read-only auth protocol does not expose account identity or subscription.
            result.detail = Some("Pi does not report subscription information.".to_owned());
            Ok(result)
        }
        ("not_ready", false)
            if matches!(
                response.reason.as_deref(),
                Some("credentials_not_configured" | "credential_not_available")
            ) =>
        {
            Ok(status(AgentKind::Pi, Some(false)))
        }
        ("not_ready", _) if response.reason.as_deref() == Some("provider_not_found") => {
            Err("Pi model provider is unavailable".to_owned())
        }
        _ => Err("Pi could not determine account readiness".to_owned()),
    }
}

pub fn codex(response: &Value) -> Result<TerminalAccountStatus, String> {
    let account = response
        .get("account")
        .ok_or("Codex did not report an account")?;
    let mut result = status(AgentKind::Codex, None);
    if account.is_null() {
        match response.get("requiresOpenaiAuth").and_then(Value::as_bool) {
            Some(true) => result.authenticated = Some(false),
            Some(false) => {
                result.detail = Some(
                    "The selected provider does not require OpenAI authentication.".to_owned(),
                );
            }
            None => return Err("Codex did not report account readiness".to_owned()),
        }
        return Ok(result);
    }
    match account.get("type").and_then(Value::as_str) {
        Some("chatgpt") => {
            result.authenticated = Some(true);
            result.auth_method = Some("chatgpt".to_owned());
            result.account = label(
                account
                    .get("email")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
            result.subscription = label(
                account
                    .get("planType")
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            );
        }
        Some("apiKey") => {
            result.authenticated = Some(true);
            result.auth_method = Some("api_key".to_owned());
        }
        Some("amazonBedrock") => {
            result.auth_method = Some("amazon_bedrock".to_owned());
            match account.get("credentialSource").and_then(Value::as_str) {
                Some("codexManaged") => result.authenticated = Some(true),
                Some("awsManaged") => {
                    result.detail = Some(
                        "Codex uses external AWS credentials; account/read does not validate them."
                            .to_owned(),
                    );
                }
                _ => return Err("Codex did not report a Bedrock credential source".to_owned()),
            }
        }
        _ => return Err("Codex reported an unsupported account type".to_owned()),
    }
    Ok(result)
}
