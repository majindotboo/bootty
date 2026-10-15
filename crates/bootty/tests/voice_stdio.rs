//! Public speech helper failures preserve a usable composer protocol without opening audio.
use bootty_host::speech::SpeechEvent;
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

#[rstest]
#[case("{\"action\":\"listen\",\"voice\":false}\n")]
#[case("{\"action\":\"listen\",\"voice\":false,\"unexpected\":true}\n")]
#[case(
    "{\"action\":\"account\",\"provider\":\"codex\",\"program\":\"must-not-run\",\"directory\":null}\n{\"action\":\"listen\",\"voice\":true}\n"
)]
fn rejected_start_reports_error_and_stops_cleanly(#[case] input: &str) {
    // The default Codex home is empty, so no sign-in is found.
    let codex_home = assert_fs::TempDir::new().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-voice"))
        .env_remove("OPENAI_API_KEY")
        .env("CODEX_HOME", codex_home.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<SpeechEvent>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.first(), Some(&SpeechEvent::Ready));
    assert!(matches!(events.get(1), Some(SpeechEvent::Error { message }) if !message.is_empty()));
    assert_eq!(events.last(), Some(&SpeechEvent::Stopped));
}

/// Without a current Codex sign-in, recording fails before any audio opens.
#[rstest]
#[case::signed_out(r#"{"OPENAI_API_KEY":null}"#, "Sign in to Codex to use dictation")]
#[case::expired(
    r#"{"tokens":{"access_token":"header.eyJleHAiOjF9.signature","account_id":"account"}}"#,
    "The Codex sign-in expired. Open Codex to sign in again."
)]
fn dictation_needs_a_current_codex_sign_in(#[case] auth: &str, #[case] message: &str) {
    use assert_fs::prelude::*;
    let account = assert_fs::TempDir::new().unwrap();
    account.child("auth.json").write_str(auth).unwrap();
    let input = format!(
        "{}\n{}\n",
        serde_json::json!({"action":"account","provider":"codex","program":"must-not-run","directory":account.path()}),
        serde_json::json!({"action":"listen","voice":false})
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-voice"))
        .env_remove("OPENAI_API_KEY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    let events = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str::<SpeechEvent>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        events,
        vec![
            SpeechEvent::Ready,
            SpeechEvent::Error {
                message: message.into()
            },
            SpeechEvent::Stopped
        ]
    );
}
