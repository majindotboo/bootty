use std::io::Read as _;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use bootty_host::shell_quote;
use bootty_mux::command::MuxCommand;
use bootty_mux::remote_space::{
    decode_command, encode_command, encode_stream_command, read_stream_command,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use serde_json::json;
use static_assertions::assert_impl_all;

assert_impl_all!(MuxCommand: Send, Sync);

#[derive(Arbitrary, Debug)]
struct RenameCommand {
    #[proptest(regex = ".{0,128}")]
    session_id: String,
    #[proptest(regex = ".{0,128}")]
    name: String,
}

proptest! {
    /// Property: POSIX single quoting preserves every scalar and escapes only apostrophes.
    #[test]
    fn shell_quoting_matches_the_posix_oracle(value in ".{0,256}") {
        assert_eq!(shell_quote(&value), format!("'{}'", value.replace('\'', "'\\''")));
    }

    /// Property: the wire representation and its decoded command preserve arbitrary arguments.
    #[test]
    fn space_commands_match_the_wire_oracle(model in any::<RenameCommand>()) {
        let RenameCommand { session_id, name } = model;
        let command = MuxCommand::RenameSession { session_id: session_id.clone(), name: name.clone() };
        let encoded = encode_command(&command).expect("encode command");
        let wire: serde_json::Value = serde_json::from_slice(
            &URL_SAFE_NO_PAD.decode(&encoded).expect("decode base64"),
        ).expect("decode JSON");
        assert_eq!(wire, json!({ "RenameSession": { "session_id": session_id, "name": name } }));
        assert_eq!(decode_command(&encoded).expect("decode command"), command);
        assert_eq!(read_stream_command(encode_stream_command(&command).unwrap().as_slice()).unwrap(), command);
    }

    #[test]
    fn native_pane_commands_preserve_backend_and_conversation_identities(
        session in ".{1,32}", window in ".{1,32}", pane in ".{1,32}",
        agent in proptest::option::of("native:codex:[a-z0-9]{1,32}"),
    ) {
        for command in [
            MuxCommand::ActivatePane {
                session_id: session.clone(), window_id: window,
                pane_id: pane.clone(),
            },
            MuxCommand::SetPaneNativeAgent {
                session_id: session, pane_id: pane, agent_id: agent,
            },
        ] {
            assert_eq!(decode_command(&encode_command(&command).unwrap()).unwrap(), command);
        }
    }
}

#[rstest::rstest]
fn streamed_restoration_retains_history_beyond_command_line_and_legacy_limits() {
    use bootty_mux::{
        session_snapshot::{SavedTerminalPane, SavedTerminalSession, SavedTerminalWindow},
        snapshot::MuxSessionTag,
    };
    let text = format!("\x1b[31m{}\x1b[0m", "retained output 🥟\n".repeat(12_000));
    let panes = (0..5)
        .map(|id| SavedTerminalPane {
            id: format!("pane-{id}"),
            backend_id: format!("%{id}"),
            cwd: "/tmp".into(),
            cols: 80,
            rows: 24,
            text: text.clone(),
            omitted_lines: 7,
            native_agent: None,
        })
        .collect();
    let snapshot = SavedTerminalSession {
        captured_at: 1,
        session_id: "task".into(),
        backend_id: "$0".into(),
        active_window_id: Some("window".into()),
        windows: vec![SavedTerminalWindow {
            id: "window".into(),
            backend_id: "@0".into(),
            title: "Work".into(),
            focused_pane_id: "pane-0".into(),
            layout: None,
            panes,
        }],
    };
    snapshot.validate().unwrap();
    let command = MuxCommand::RestoreSession {
        session_id: "restored".into(),
        tag: MuxSessionTag {
            identity: Some("task".into()),
            space: Some("space".into()),
        },
        snapshot,
    };
    assert!(encode_command(&command).is_err());
    let bytes = encode_stream_command(&command).unwrap();
    assert!(decode_command(&URL_SAFE_NO_PAD.encode(&bytes)).is_err());
    assert_eq!(read_stream_command(bytes.as_slice()).unwrap(), command);
}

#[rstest::rstest]
#[case::oversized(std::io::repeat(b' ').take(32 * 1024 * 1024 + 1))]
#[case::truncated(std::io::repeat(b'{').take(1))]
fn streamed_commands_reject_oversized_and_incomplete_requests(
    #[case] reader: std::io::Take<std::io::Repeat>,
) {
    assert!(read_stream_command(reader).is_err());
}
