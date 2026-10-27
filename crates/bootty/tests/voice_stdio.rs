//! Public speech helper failures preserve a usable composer protocol without opening audio.
use bootty_host::speech::SpeechEvent;
use pretty_assertions::assert_eq;
use rstest::rstest;
use std::{
    io::Write as _,
    process::{Command, Stdio},
};

#[rstest]
#[case("{\"action\":\"listen\",\"voice\":false}\n", true)]
#[case("{\"action\":\"listen\",\"voice\":false,\"unexpected\":true}\n", false)]
#[case("{\"action\":\"answer\",\"sdp\":\"no-active-call\"}\n", false)]
fn rejected_start_reports_error_and_stops_cleanly(#[case] input: &str, #[case] executable: bool) {
    let output = if executable {
        let mut child = Command::new(env!("CARGO_BIN_EXE_bootty-voice"))
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
        String::from_utf8(output.stdout).unwrap()
    } else {
        run_voice(input.to_owned()).unwrap()
    };
    let events = output
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
    let output = run_voice(input).unwrap();
    let events = output
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
#[case(vec![serde_json::json!({"type":"state","state":"failed"})], SpeechEvent::Error { message: "Voice connection failed. Check your network and try again.".into() }, true)]
#[case(vec![serde_json::json!({"type":"state","state":"disconnected"})], SpeechEvent::Error { message: "Voice connection was lost. Check your network and try again.".into() }, false)]
#[case(vec![serde_json::json!({"type":"state","state":"closed"})], SpeechEvent::Closed, false)]
#[case(vec![serde_json::json!({"type":"error","message":"Cannot apply the remote SDP"})], SpeechEvent::Error { message: "Voice audio host: Cannot apply the remote SDP".into() }, false)]
#[case(vec![serde_json::json!({"type":"data","message":{"type":"error","error":{"message":"Voice service unavailable"}}})], SpeechEvent::Error { message: "Voice provider: Voice service unavailable".into() }, false)]
#[case(
    vec![serde_json::json!({"type":"error","message":"speaker stream error: A buffer underrun or overrun occurred."}),
         serde_json::json!({"type":"state","state":"connected"})],
    SpeechEvent::Listening,
    false
)]
#[case(
    vec![serde_json::json!({"type":"error","message":"speaker stream error: A buffer underrun or overrun occurred."}),
         serde_json::json!({"type":"error","message":"Speaker device was removed"})],
    SpeechEvent::Error { message: "Voice audio host: Speaker device was removed".into() },
    false
)]
fn voice_media_reports_connection_loss_close_and_provider_errors(
    #[case] events: Vec<serde_json::Value>,
    #[case] expected: SpeechEvent,
    #[case] executable: bool,
) {
    use assert_fs::prelude::*;
    use std::io::{BufRead as _, BufReader};
    let root = assert_fs::TempDir::new().unwrap();
    let host = root.child("voice-host");
    let frames = events
        .iter()
        .map(serde_json::Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    bootty_test_support::write_executable(host.path(),
        "#!/bin/sh\nprintf '%s\n' '{\"type\":\"ready\",\"version\":6}'\nIFS= read -r command\nprintf '%s\n' \"$BOOTTY_TEST_VOICE_EVENT\"\ncat >/dev/null\n",
    ).unwrap();
    let (mut input, requests) = std::os::unix::net::UnixStream::pair().unwrap();
    let (responses, output) = std::os::unix::net::UnixStream::pair().unwrap();
    let mut host = Command::new(host.path());
    host.env("BOOTTY_TEST_VOICE_EVENT", &frames);
    let (audio, mut peer) = std::os::unix::net::UnixStream::pair().unwrap();
    let fixture = (!executable).then(|| {
        let mut commands = BufReader::new(peer.try_clone().unwrap());
        std::thread::spawn(move || {
            writeln!(peer, "{{\"type\":\"ready\",\"version\":6}}").unwrap();
            let mut command = String::new();
            commands.read_line(&mut command).unwrap();
            writeln!(peer, "{frames}").unwrap();
            loop {
                command.clear();
                if commands.read_line(&mut command).unwrap() == 0 || command.contains("shutdown") {
                    break;
                }
            }
        })
    });
    let audio_input = audio.try_clone().unwrap();
    let audio = std::sync::Mutex::new(Some((audio_input, audio)));
    let handler = std::thread::spawn(move || {
        if executable {
            bootty_agents::voice::run_with_io(BufReader::new(requests), responses, host)
        } else {
            bootty_agents::voice::run_with_audio(BufReader::new(requests), responses, move || {
                let (input, output) = audio
                    .lock()
                    .map_err(|_| "audio transport lock")?
                    .take()
                    .ok_or("audio transport already opened")?;
                Ok(bootty_agents::voice::AudioHost::from_io(
                    BufReader::new(input),
                    output,
                ))
            })
        }
    });
    writeln!(input, "{{\"action\":\"listen\",\"voice\":true}}").unwrap();
    let mut output = BufReader::new(output).lines();
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
    handler.join().unwrap().unwrap();
    if let Some(fixture) = fixture {
        fixture.join().unwrap();
    }
}

#[derive(Clone, Default)]
struct Output(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Output {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .map_err(|_| std::io::Error::other("output lock"))?
            .write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_voice(input: String) -> anyhow::Result<String> {
    let output = Output::default();
    bootty_agents::voice::run_with_io(
        std::io::Cursor::new(input),
        output.clone(),
        Command::new("must-not-run"),
    )?;
    let bytes = output
        .0
        .lock()
        .map_err(|_| anyhow::anyhow!("output lock"))?
        .clone();
    Ok(String::from_utf8(bytes)?)
}
