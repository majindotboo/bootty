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
#[case("{\"action\":\"answer\",\"sdp\":\"no-active-call\"}\n")]
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

/// Audio-host failures terminate capture and retain their actual diagnostic.
#[cfg(unix)]
#[rstest]
#[case(vec![serde_json::json!({"type":"state","state":"failed"})], SpeechEvent::Error { message: "Voice connection failed. Check your network and try again.".into() })]
#[case(vec![serde_json::json!({"type":"state","state":"disconnected"})], SpeechEvent::Error { message: "Voice connection was lost. Check your network and try again.".into() })]
#[case(vec![serde_json::json!({"type":"state","state":"closed"})], SpeechEvent::Closed)]
#[case(vec![serde_json::json!({"type":"error","message":"Cannot apply the remote SDP"})], SpeechEvent::Error { message: "Voice audio host: Cannot apply the remote SDP".into() })]
#[case(vec![serde_json::json!({"type":"data","message":{"type":"error","error":{"message":"Voice service unavailable"}}})], SpeechEvent::Error { message: "Voice provider: Voice service unavailable".into() })]
#[case(
    vec![serde_json::json!({"type":"error","message":"speaker stream error: A buffer underrun or overrun occurred."}),
         serde_json::json!({"type":"state","state":"connected"})],
    SpeechEvent::Listening
)]
#[case(
    vec![serde_json::json!({"type":"error","message":"speaker stream error: A buffer underrun or overrun occurred."}),
         serde_json::json!({"type":"error","message":"Speaker device was removed"})],
    SpeechEvent::Error { message: "Voice audio host: Speaker device was removed".into() }
)]
fn voice_media_reports_connection_loss_close_and_provider_errors(
    #[case] events: Vec<serde_json::Value>,
    #[case] expected: SpeechEvent,
) {
    use assert_fs::prelude::*;
    use std::io::{BufRead as _, BufReader};
    let root = assert_fs::TempDir::new().unwrap();
    let helper = root.child("bootty-voice");
    bootty_test_support::place_executable(env!("CARGO_BIN_EXE_bootty-voice"), helper.path())
        .unwrap();
    let host = root.child("voice-host");
    bootty_test_support::write_executable(
        host.path(),
        r#"#!/bin/sh
printf '%s\n' '{"type":"ready","version":6}'
IFS= read -r command
printf '%s\n' "$BOOTTY_TEST_VOICE_EVENT"
cat >/dev/null
"#,
    )
    .unwrap();
    let mut child = Command::new(helper.path())
        .env(
            "BOOTTY_TEST_VOICE_EVENT",
            events
                .iter()
                .map(serde_json::Value::to_string)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    writeln!(input, "{{\"action\":\"listen\",\"voice\":true}}").unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut next =
        || serde_json::from_str::<SpeechEvent>(&output.next().unwrap().unwrap()).unwrap();
    assert_eq!(next(), SpeechEvent::Ready);
    assert_eq!(next(), expected);
    if matches!(expected, SpeechEvent::Error { .. }) {
        assert_eq!(next(), SpeechEvent::Stopped);
    }
    if expected == SpeechEvent::Listening {
        writeln!(input, "{{\"action\":\"stop\"}}").unwrap();
        assert_eq!(next(), SpeechEvent::Stopped);
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}
