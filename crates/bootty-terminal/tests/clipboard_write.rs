use base64::{Engine as _, engine::general_purpose::STANDARD};
use bootty_terminal::{
    clipboard_write::{ClipboardResult, ClipboardWrite},
    geometry::TerminalGeometry,
    terminal_engine::TerminalEngine,
    terminal_side_effect::TerminalSideEffect,
};
use pretty_assertions::assert_eq;
use proptest::prelude::*;
use rstest::rstest;
proptest! {
    #[test]
    fn arbitrary_chunk_boundaries_preserve_image(bytes in proptest::collection::vec(any::<u8>(), 1..12000), chunk in 1usize..4097) {
        let mut parser = ClipboardWrite::default();
        prop_assert!(parser.feed(b"type=write:id=client", true).is_none());
        for chunk in bytes.chunks(chunk) {
            let packet = format!("type=wdata:mime={};{}", STANDARD.encode("image/png"), STANDARD.encode(chunk));
            prop_assert!(parser.feed(packet.as_bytes(), true).is_none());
        }
        let Some(ClipboardResult::Image(image)) = parser.feed(b"type=wdata", true) else { panic!("missing image"); };
        prop_assert_eq!(image.data, bytes); prop_assert_eq!(image.id, "client");
        prop_assert!(parser.feed(b"type=wdata", true).is_none());
    }
}
#[rstest]
#[case(false, b"type=write:id=denied".as_slice(), "EPERM")]
#[case(true, b"type=write:id=denied:loc=primary".as_slice(), "ENOSYS")]
fn denied_transfers_ignore_following_chunks(
    #[case] allowed: bool,
    #[case] start: &[u8],
    #[case] status: &str,
) {
    let mut parser = ClipboardWrite::default();
    assert_eq!(
        parser.feed(start, allowed),
        Some(ClipboardResult::Reply(
            format!("\x1b]5522;type=write:status={status}:id=denied\x1b\\").into_bytes()
        ))
    );
    assert!(
        parser
            .feed(b"type=wdata:mime=aW1hZ2UvcG5n;YWJj", true)
            .is_none()
    );
    assert!(parser.feed(b"type=wdata", true).is_none());
}
#[rstest]
fn protocol_error_aborts_until_next_start() {
    let mut parser = ClipboardWrite::default();
    parser.feed(b"type=write", true);
    assert!(matches!(
        parser.feed(b"type=wdata:mime=aW1hZ2UvcG5n;!!!", true),
        Some(ClipboardResult::Reply(_))
    ));
    assert!(parser.feed(b"type=wdata", true).is_none());
    parser.feed(b"type=write", true);
    assert!(parser.is_active());
    parser.reset();
    assert!(!parser.is_active());
}
#[rstest]
#[case(1)]
#[case(13)]
#[case(4096)]
fn engine_streaming_and_replay_keep_clipboard_effects_separate(#[case] chunk: usize) {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    })
    .unwrap();
    let packet = b"\x1b]5522;type=write:id=x\x1b\\";
    for bytes in packet.chunks(chunk) {
        engine.write_vt(bytes);
    }
    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::ClipboardPacket(
            b"type=write:id=x".to_vec()
        )]
    );
    engine.write_vt_without_pty_responses(packet);
    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::ClipboardReset]
    );
    engine.write_vt(b"\x1b]5522;type=wdata;");
    for _ in 0..32 {
        engine.write_vt(&vec![b'x'; 4096]);
    }
    engine.write_vt(b"\x1b");
    engine.write_vt(b"\\");
    engine.write_vt(packet);
    assert_eq!(
        engine.drain_side_effects(),
        vec![
            TerminalSideEffect::ClipboardPacket(Vec::new()),
            TerminalSideEffect::ClipboardPacket(b"type=write:id=x".to_vec())
        ]
    );
}

#[rstest]
fn replay_cancels_live_clipboard_assembly_even_without_clipboard_data() {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols: 80,
        rows: 24,
        cell_width: 8,
        cell_height: 16,
    })
    .unwrap();
    engine.write_vt(b"\x1b]5522;type=write\x07");
    engine.drain_side_effects();
    engine.write_vt_without_pty_responses(b"restored screen");
    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::ClipboardReset]
    );
    engine.write_vt_without_pty_responses(b"another frame");
    assert_eq!(
        engine.drain_side_effects(),
        Vec::<bootty_terminal::terminal_engine::TerminalSideEffect>::new()
    );
}
