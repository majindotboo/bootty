#![cfg(test)]
//! Codex speech runs on the Codex sign-in already on this machine, and nothing else.

use assert_fs::{TempDir, prelude::*};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use rstest::rstest;

/// A JWT-shaped token that expires `seconds` from now.
fn token(seconds: i64) -> String {
    let now = i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs(),
    )
    .unwrap();
    let claims = serde_json::json!({"exp": now.saturating_add(seconds)}).to_string();
    format!("header.{}.signature", URL_SAFE_NO_PAD.encode(claims))
}

#[rstest]
#[case::subscription(Some(serde_json::json!({"auth_mode":"chatgpt","OPENAI_API_KEY":null,
    "tokens":{"access_token":token(3600),"account_id":"account"}})), true)]
#[case::expired_subscription(Some(serde_json::json!({"auth_mode":"chatgpt",
    "tokens":{"access_token":token(-60),"account_id":"account"}})), false)]
#[case::api_key(Some(serde_json::json!({"auth_mode":"apikey","OPENAI_API_KEY":"sk-test"})), true)]
#[case::signed_out(Some(serde_json::json!({"OPENAI_API_KEY":null})), false)]
#[case::no_sign_in(None, false)]
fn codex_speech_needs_a_current_codex_sign_in(
    #[case] auth: Option<serde_json::Value>,
    #[case] available: bool,
) {
    let home = TempDir::new().unwrap();
    if let Some(auth) = auth {
        home.child("auth.json")
            .write_str(&auth.to_string())
            .unwrap();
    }
    assert_eq!(
        bootty_agents::voice::availability(Some(home.path())).codex,
        available
    );
}
