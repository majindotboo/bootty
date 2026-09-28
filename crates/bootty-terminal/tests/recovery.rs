use bootty_terminal::{
    geometry::TerminalGeometry, terminal_engine::TerminalEngine, terminal_frame::RenderFrame,
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn rows(frame: &RenderFrame) -> Vec<String> {
    (0..frame.rows)
        .map(|row| {
            frame
                .cells
                .iter()
                .filter(|cell| cell.y == row)
                .flat_map(|cell| frame.cell_text(cell).iter().copied())
                .collect()
        })
        .collect()
}

#[rstest]
#[case(b"\x1b[3;1H\x1b[2K\x1b[1;1H\x1b[2KUpdated transcript")]
#[case(b"\x1b]10;rgb:aaaa/bbbb/cccc\x1b\\\x1b[3;1H\x1b[2K")]
fn recovery_preserves_live_continuation_at_every_byte_boundary(#[case] update: &[u8]) {
    let geometry = TerminalGeometry {
        cols: 40,
        rows: 5,
        cell_width: 10,
        cell_height: 20,
    };
    let screen = b"\x1b[2J\x1b[1;1HCompacted from 255,858 tokens\x1b[3;1HCursor Images Codex Native\x1b[1;1H";
    let mut reference = TerminalEngine::new(geometry).unwrap();
    reference.write_vt(screen);
    reference.write_vt(update);
    let expected = rows(reference.extract_frame().unwrap());
    for split in 0..=update.len() {
        let mut replay = TerminalEngine::new(geometry).unwrap();
        let keyframe = [screen.as_slice(), &update[..split]].concat();
        replay.write_vt_without_pty_responses(&keyframe);
        replay.write_vt(&update[split..]);
        assert_eq!(
            rows(replay.extract_frame().unwrap()),
            expected,
            "split {split}"
        );
    }
}
