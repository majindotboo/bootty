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
    "{\"action\":\"account\",\"provider\":\"unsupported\",\"program\":\"must-not-run\",\"directory\":null}\n{\"action\":\"listen\",\"voice\":true}\n"
)]
fn rejected_start_reports_error_and_stops_cleanly(#[case] input: &str) {
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

#[rstest]
#[case(false)]
#[case(true)]
fn subscription_tokens_do_not_authenticate_public_audio(#[case] voice: bool) {
    use assert_fs::prelude::*;
    let account = assert_fs::TempDir::new().unwrap();
    account
        .child("auth.json")
        .write_str(
            r#"{"tokens":{"access_token":"subscription-token","account_id":"private-account"}}"#,
        )
        .unwrap();
    let input = format!(
        "{}\n{}\n",
        serde_json::json!({"action":"account","provider":"codex","program":"must-not-run","directory":account.path()}),
        serde_json::json!({"action":"listen","voice":voice})
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
                message: "Configure an OpenAI API key to use dictation and voice".into()
            },
            SpeechEvent::Stopped
        ]
    );
}
