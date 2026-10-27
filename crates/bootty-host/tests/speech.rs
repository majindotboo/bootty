#![cfg(unix)]
#![cfg(test)]
use assert_fs::TempDir;
use bootty_host::speech::{SpeechCommand, SpeechEvent, SpeechPeer, SpeechSession};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn dictation_and_voice_deliver_events_without_recording_audio() {
    let (session, mut events) =
        SpeechSession::start_with("fixture-speech".into(), |program| Ok(peer(&program)));
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Ready));
    session
        .send(SpeechCommand::Listen { voice: false })
        .unwrap();
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Listening));
    // Only loudness crosses for the waveform, never the audio itself.
    assert_eq!(
        events.blocking_recv(),
        Some(SpeechEvent::Level { rms: 1200 })
    );
    assert_eq!(
        events.blocking_recv(),
        Some(SpeechEvent::Partial {
            text: "Recognized words".into()
        })
    );
    session.send(SpeechCommand::Stop).unwrap();
    assert_eq!(
        events.blocking_recv(),
        Some(SpeechEvent::Final {
            text: "Recognized words".into()
        })
    );
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Stopped));
    session
        .send(SpeechCommand::Answer { sdp: "v=0".into() })
        .unwrap();
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Listening));
    assert!(
        session
            .send(SpeechCommand::Answer {
                sdp: "x".repeat(32_769)
            })
            .is_err()
    );
    drop(session);
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Closed));
}

#[rstest]
#[case::invalid_json(b"invalid-json\n".to_vec())]
#[case::oversized(format!("{}\n", "x".repeat(65_537)).into_bytes())]
fn invalid_speech_frames_are_reported_and_the_stream_closes(#[case] response: Vec<u8>) {
    let (session, mut events) = SpeechSession::start_with("fixture-speech".into(), move |_| {
        Ok(SpeechPeer::new(
            std::io::sink(),
            std::io::Cursor::new(response),
            || {},
        ))
    });
    assert!(matches!(
        events.blocking_recv(),
        Some(SpeechEvent::Error { .. })
    ));
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Closed));
    drop(session);
}

#[rstest]
fn a_missing_speech_helper_reports_spawn_failure_and_closes() {
    let root = TempDir::new().unwrap();
    let (session, mut events) = SpeechSession::start(root.path().join("missing-speech-helper"));
    assert!(matches!(
        events.blocking_recv(),
        Some(SpeechEvent::Error { .. })
    ));
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Closed));
    drop(session);
}

fn peer(program: &std::path::Path) -> SpeechPeer {
    use std::{
        io::{BufRead as _, BufReader, Write as _},
        net::Shutdown,
        os::unix::net::UnixStream,
    };
    assert_eq!(program, std::path::Path::new("fixture-speech"));
    let (host, mut provider) = UnixStream::pair().unwrap();
    let input = host.try_clone().unwrap();
    let control = host.try_clone().unwrap();
    let worker = std::thread::spawn(move || {
        let mut requests = BufReader::new(provider.try_clone().unwrap());
        let mut emit = |event| {
            serde_json::to_writer(&mut provider, &event).unwrap();
            provider.write_all(b"\n").unwrap();
        };
        emit(SpeechEvent::Ready);
        loop {
            let mut line = String::new();
            match requests.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            match serde_json::from_str::<SpeechCommand>(&line).unwrap() {
                SpeechCommand::Listen { .. } => {
                    emit(SpeechEvent::Listening);
                    emit(SpeechEvent::Level { rms: 1200 });
                    emit(SpeechEvent::Partial {
                        text: "Recognized words".into(),
                    });
                }
                SpeechCommand::Stop => {
                    emit(SpeechEvent::Final {
                        text: "Recognized words".into(),
                    });
                    emit(SpeechEvent::Stopped);
                }
                SpeechCommand::Answer { .. } => emit(SpeechEvent::Listening),
                command => panic!("unexpected fixture command: {command:?}"),
            }
        }
    });
    SpeechPeer::new(input, host, move || {
        control.shutdown(Shutdown::Both).unwrap();
        worker.join().unwrap();
    })
}
