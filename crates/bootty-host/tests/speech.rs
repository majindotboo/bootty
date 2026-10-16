#![cfg(unix)]
use assert_fs::TempDir;
use bootty_host::speech::{SpeechCommand, SpeechEvent, SpeechSession};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn dictation_and_voice_deliver_events_without_recording_audio() {
    let root = TempDir::new().unwrap();
    let program = root.path().join("speech.py");
    bootty_test_support::write_executable(
        &program,
        r"#!/usr/bin/env python3
import json,sys
def emit(event,**fields): print(json.dumps({'event':event,**fields}),flush=True)
emit('ready')
for line in sys.stdin:
 command=json.loads(line)
 if command['action']=='listen': emit('listening'); emit('partial',text='Recognized words')
 elif command['action']=='stop': emit('final',text='Recognized words'); emit('stopped')
 elif command['action']=='answer': emit('listening')
",
    )
    .unwrap();
    let (session, mut events) = SpeechSession::start(program);
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Ready));
    session
        .send(SpeechCommand::Listen { voice: false })
        .unwrap();
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Listening));
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
#[case("print('invalid-json',flush=True)")]
#[case("print('x'*65537,flush=True)")]
fn invalid_speech_frames_are_reported_and_the_stream_closes(#[case] response: &str) {
    let root = TempDir::new().unwrap();
    let program = root.path().join("speech.py");
    bootty_test_support::write_executable(
        &program,
        format!("#!/usr/bin/env python3\n{response}\n"),
    )
    .unwrap();
    let (session, mut events) = SpeechSession::start(program);
    assert!(matches!(
        events.blocking_recv(),
        Some(SpeechEvent::Error { .. })
    ));
    assert_eq!(events.blocking_recv(), Some(SpeechEvent::Closed));
    drop(session);
}
