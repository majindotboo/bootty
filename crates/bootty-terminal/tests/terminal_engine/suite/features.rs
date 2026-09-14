use anyhow::{Context, Result};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use proptest_derive::Arbitrary;
use unicode_width::UnicodeWidthChar;

use super::super::*;

const fn rgb(r: u8, g: u8, b: u8) -> RgbColor {
    RgbColor { r, g, b }
}

#[derive(Arbitrary, Debug)]
struct CursorCase {
    #[proptest(strategy = "1u16..80")]
    cols: u16,
    #[proptest(strategy = "1u16..40")]
    rows: u16,
    #[proptest(strategy = "0u16..200")]
    y: u16,
    #[proptest(strategy = "0u16..200")]
    x: u16,
    #[proptest(strategy = "0u16..200")]
    delta: u16,
    direction: CursorDirection,
}

#[derive(Arbitrary, Clone, Copy, Debug)]
enum CursorDirection {
    Up,
    Down,
    Right,
    Left,
}

fn occupied_cells(frame: &RenderFrame) -> Result<Vec<(char, u16, u16)>> {
    frame
        .cells
        .iter()
        .filter(|cell| cell.text_len > 0)
        .map(|cell| {
            Ok((
                *frame
                    .text
                    .get(cell.text_start)
                    .context("occupied cell text")?,
                cell.x,
                cell.y,
            ))
        })
        .collect()
}

fn visible_text_rows(frame: &RenderFrame) -> Result<Vec<String>> {
    let mut rows = vec![String::new(); usize::from(frame.rows)];
    for cell in frame.cells.iter().filter(|cell| cell.text_len > 0) {
        let row = rows
            .get_mut(usize::from(cell.y))
            .context("visible cell row")?;
        while text_cell_width(row.chars()) < usize::from(cell.x) {
            row.push(' ');
        }
        row.extend(frame.cell_text(cell));
    }
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    Ok(rows)
}

fn text_cell_width(chars: impl Iterator<Item = char>) -> usize {
    chars
        .map(|ch| UnicodeWidthChar::width(ch).unwrap_or(0))
        .sum()
}

proptest! {
    #[test]
    fn synchronized_output_observation_survives_fragmented_input(
        prefix in "[a-z]{0,64}",
        suffix in "[a-z]{0,64}",
        chunk in 1usize..24,
        enabled in any::<bool>(),
    ) {
        let mut engine = TerminalEngine::new(test_geometry(80, 2)).unwrap();
        // A failed prefix ending in ESC must still recognize the next start.
        let control = if enabled { "\x1b[?20\x1b[?2026h\x1b[?2026l" } else { "\x1b[?2025h" };
        let stream = format!("{prefix}{control}{suffix}");
        let mut observed = false;
        for bytes in stream.as_bytes().chunks(chunk) {
            engine.write_vt(bytes);
            observed |= engine.take_synchronized_output_observed();
        }
        prop_assert_eq!(observed, enabled);
        prop_assert!(!engine.take_synchronized_output_observed());
    }
}

#[rstest::rstest]
#[case(1)]
#[case(7)]
#[case(1024)]
fn terminal_engine_preserves_style_changes_across_write_boundaries(#[case] chunk: usize) {
    let mut engine = TerminalEngine::new(test_geometry(8, 1)).expect("test operation succeeds");
    let bytes = b"\x1b[1;3;4mA\x1b[1;3;4mB\x1b[22mC\x1b[0mD";
    for part in bytes.chunks(chunk) {
        engine.write_vt(part);
    }
    let frame = engine.extract_frame().expect("test operation succeeds");
    let cells = collect_visible_cells(frame, |text, cell| {
        (
            text,
            cell.style.bold,
            cell.style.italic,
            cell.style.underline,
        )
    })
    .expect("visible cells");
    assert_eq!(
        cells,
        vec![
            ('A', true, true, Underline::Single),
            ('B', true, true, Underline::Single),
            ('C', false, true, Underline::Single),
            ('D', false, false, Underline::None),
        ]
    );
}

#[test]
fn terminal_engine_preserves_dense_sgr_cell_frame() {
    let mut engine = TerminalEngine::new(test_geometry(4, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b[H\x1b[38;5;101;48;5;202;1;3;4mA\x1b[38;5;102;48;5;201;1;3;4mB");
    let frame = engine.extract_frame().expect("test operation succeeds");
    let styled_cells = collect_visible_cells(frame, |text, cell| {
        (
            text,
            cell.fg,
            cell.bg,
            cell.style.bold,
            cell.style.italic,
            cell.style.underline,
        )
    })
    .expect("visible cells");

    assert_eq!(styled_cells.len(), 2);
    assert_eq!(styled_cells[0].0, 'A');
    assert_eq!(styled_cells[1].0, 'B');
    for (_, fg, bg, bold, italic, underline) in &styled_cells {
        assert!(fg.is_some());
        assert!(bg.is_some());
        assert!(*bold);
        assert!(*italic);
        assert_eq!(*underline, Underline::Single);
    }
    assert_ne!(styled_cells[0].1, styled_cells[1].1);
    assert_ne!(styled_cells[0].2, styled_cells[1].2);
}

#[test]
fn terminal_engine_collapses_split_repeated_cursor_home_controls() {
    let mut engine = TerminalEngine::new(test_geometry(4, 1)).expect("test operation succeeds");

    engine.write_vt(b"abcd");
    engine.write_vt(b"\x1b[H\x1b");
    engine.write_vt(b"[H\x1b[");
    engine.write_vt(b"HZ");

    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(
        visible_text_rows(frame).expect("frame matches expected behavior"),
        vec!["Zbcd".to_owned()]
    );
}

#[test]
fn terminal_engine_collects_osc52_clipboard_text() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]52;c;aGVsbG8gdG11eA==\x07");

    assert_eq!(
        drain_clipboard_texts(&mut engine),
        vec!["hello tmux".to_owned()]
    );
    assert_eq!(
        drain_clipboard_texts(&mut engine),
        Vec::<std::string::String>::new()
    );
}

#[test]
fn terminal_engine_collects_split_prefix_osc52_clipboard_text() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]");
    assert_eq!(
        drain_clipboard_texts(&mut engine),
        Vec::<std::string::String>::new()
    );

    engine.write_vt(b"52;c;aGVsbG8gdG11eA==\x07");

    assert_eq!(
        drain_clipboard_texts(&mut engine),
        vec!["hello tmux".to_owned()]
    );
}

#[test]
fn terminal_engine_collects_split_prefix_iterm2_report_cell_size() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]133");
    assert_eq!(
        engine.drain_side_effects(),
        Vec::<bootty_terminal::terminal_engine::TerminalSideEffect>::new()
    );

    engine.write_vt(b"7;ReportCellSize\x1b\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::ReportCellSize]
    );
}

#[test]
fn terminal_engine_collects_split_terminator_iterm2_report_cell_size() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]1337;ReportCellSize\x1b");
    assert_eq!(
        engine.drain_side_effects(),
        Vec::<bootty_terminal::terminal_engine::TerminalSideEffect>::new()
    );

    engine.write_vt(b"\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::ReportCellSize]
    );
}

#[test]
fn terminal_engine_collects_split_tmux_passthrough_osc52_clipboard_text() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1bPtm");
    assert_eq!(
        drain_clipboard_texts(&mut engine),
        Vec::<std::string::String>::new()
    );

    engine.write_vt(b"ux;\x1b\x1b]52;c;aGVsbG8gdG11eA==\x07\x1b");
    assert_eq!(
        drain_clipboard_texts(&mut engine),
        Vec::<std::string::String>::new()
    );

    engine.write_vt(b"\\");

    assert_eq!(
        drain_clipboard_texts(&mut engine),
        vec!["hello tmux".to_owned()]
    );
}
#[test]
fn terminal_engine_collects_tmux_passthrough_osc52_clipboard_text() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1bPtmux;\x1b\x1b]52;c;aGVsbG8gdG11eA==\x07\x1b\\");

    assert_eq!(
        drain_clipboard_texts(&mut engine),
        vec!["hello tmux".to_owned()]
    );
}

#[test]
fn terminal_engine_collects_split_osc52_clipboard_text() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]52;c;c3Bs");
    assert_eq!(
        drain_clipboard_texts(&mut engine),
        Vec::<std::string::String>::new()
    );
    engine.write_vt(b"aXQ=\x1b\\");

    assert_eq!(drain_clipboard_texts(&mut engine), vec!["split".to_owned()]);
}

#[test]
fn terminal_engine_collects_osc52_clipboard_query() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]52;c;?\x07");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::ClipboardQuery {
            selection: "c".to_owned()
        }]
    );
}

#[test]
fn terminal_engine_collects_window_title_side_effect() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]2;bootty title\x1b\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::WindowTitle("bootty title".to_owned())]
    );

    engine.write_vt(b"\x1b]0;bootty zero title\x07");
    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::WindowTitle(
            "bootty zero title".to_owned()
        )]
    );
}

#[test]
fn terminal_engine_collects_desktop_notification_side_effect() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]777;notify;Build;Done\x1b\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::DesktopNotification {
            title: "Build".to_owned(),
            body: "Done".to_owned()
        }]
    );
}

#[test]
fn terminal_engine_collects_raw_protocol_side_effects() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]1;bootty icon\x1b\\\x1b]9;4;50\x1b\\\x1b]66;s=2;big\x1b\\\x1b]133;A\x1b\\\x1b]1337;File=name\x1b\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![
            TerminalSideEffect::WindowIcon("bootty icon".to_owned()),
            TerminalSideEffect::ConEmuProgress {
                state: "normal".to_owned(),
                value: Some(50),
            },
            TerminalSideEffect::KittyTextSizing("s=2;big".to_owned()),
            TerminalSideEffect::ShellLifecycle(
                bootty_terminal::shell_lifecycle::ShellEvent::PromptStart
            ),
            TerminalSideEffect::Iterm2File("File=name".to_owned()),
        ]
    );
}

#[test]
fn terminal_engine_treats_bare_osc9_progress_state_as_indeterminate() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]9;4;3\x07");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::ConEmuProgress {
            state: "indeterminate".to_owned(),
            value: None,
        }]
    );
}

#[test]
fn terminal_engine_handles_iterm2_copy_open_and_reports() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]1337;Copy=aGVsbG8=\x1b\\");
    engine.write_vt(
        b"\x1b]1337;CopyToClipboard=clipboard\x1b\\copied\x1b[31m text\x1b]1337;EndCopy\x1b\\",
    );
    engine.write_vt(b"\x1b]1337;OpenURL=aHR0cHM6Ly9leGFtcGxlLmNvbQ==\x1b\\");
    engine.write_vt(b"\x1b]1337;ReportCellSize\x1b\\");
    engine.write_vt(b"\x1b]1337;ReportVariable=c2Vzc2lvbi5uYW1l\x1b\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![
            TerminalSideEffect::ClipboardWrite("hello".to_owned()),
            TerminalSideEffect::Iterm2Control("CopyToClipboard=clipboard".to_owned()),
            TerminalSideEffect::ClipboardWrite("copied text".to_owned()),
            TerminalSideEffect::OpenUrl("https://example.com".to_owned()),
            TerminalSideEffect::ReportCellSize,
            TerminalSideEffect::ReportVariable("session.name".to_owned()),
        ]
    );
}

#[test]
fn terminal_engine_collects_split_iterm2_copy_capture() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]1337;CopyToClipboard=clipboard\x1b\\");
    engine.write_vt(b"copied ");
    engine.write_vt(b"\x1b[31");
    engine.write_vt(b"mred\x1b[0m text");
    engine.write_vt(b"\x1b]1337;EndCopy\x1b\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![
            TerminalSideEffect::Iterm2Control("CopyToClipboard=clipboard".to_owned()),
            TerminalSideEffect::ClipboardWrite("copied red text".to_owned()),
        ]
    );
}

#[test]
fn terminal_engine_preserves_malformed_iterm_report_variable_as_control() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]1337;ReportVariable=not base64!\x1b\\");

    assert_eq!(
        engine.drain_side_effects(),
        vec![TerminalSideEffect::Iterm2Control(
            "ReportVariable=not base64!".to_owned()
        )]
    );
}

#[test]
fn terminal_engine_extracts_osc8_hyperlink_uri() {
    let mut engine = TerminalEngine::new(test_geometry(4, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        frame.cells[0].hyperlink.as_deref(),
        Some("https://example.com")
    );
}

#[test]
fn terminal_engine_search_viewport_moves_to_scrollback_match() {
    let mut engine = TerminalEngine::new_with_scrollback(
        test_geometry(16, 3),
        TerminalColorConfig::default(),
        1_000,
    )
    .expect("test operation succeeds");
    engine.write_vt(b"first\r\ntarget needle\r\nthird\r\nfourth\r\nfifth");

    assert!(
        !visible_text_rows(engine.extract_frame().expect("test operation succeeds"))
            .expect("frame matches expected behavior")
            .iter()
            .any(|row| row.contains("target needle"))
    );

    assert!(
        engine
            .search_viewport("target needle", TerminalSearchDirection::Previous)
            .expect("test operation succeeds")
    );

    assert!(
        visible_text_rows(engine.extract_frame().expect("test operation succeeds"))
            .expect("frame matches expected behavior")
            .iter()
            .any(|row| row.contains("target needle"))
    );
}

#[test]
fn terminal_engine_search_viewport_highlights_visible_match() {
    let mut engine = TerminalEngine::new(test_geometry(16, 3)).expect("test operation succeeds");
    engine.write_vt(b"one items\r\ntwo\r\nthree");

    assert!(
        engine
            .search_viewport("items", TerminalSearchDirection::Current)
            .expect("test operation succeeds")
    );

    assert_eq!(
        engine
            .extract_frame()
            .expect("test operation succeeds")
            .search_matches,
        vec![FrameSelection {
            row: 0,
            start_col: 4,
            end_col: 8
        }]
    );
}

#[test]
fn terminal_engine_search_viewport_matches_wrapped_scrollback_line() {
    let mut engine = TerminalEngine::new_with_scrollback(
        test_geometry(8, 3),
        TerminalColorConfig::default(),
        1_000,
    )
    .expect("test operation succeeds");
    engine.write_vt(b"before\r\nwrapped needle continues\r\nafter\r\ntail\r\nend");

    assert!(
        engine
            .search_viewport("needle continues", TerminalSearchDirection::Previous)
            .expect("test operation succeeds")
    );

    let rows = visible_text_rows(engine.extract_frame().expect("test operation succeeds"))
        .expect("frame matches expected behavior");
    assert!(
        rows.iter()
            .any(|row| row.contains("needle") || row.contains("continues")),
        "search should scroll to the wrapped matching line, got {rows:?}"
    );
}

const fn test_geometry(cols: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry {
        cols,
        rows,
        cell_width: 8,
        cell_height: 16,
    }
}

fn assert_visible_text_rows(frame: &RenderFrame, expected: &[&str]) -> Result<()> {
    let actual = visible_text_rows(frame)?;
    anyhow::ensure!(
        actual == expected,
        "visible rows differ: {actual:?}, expected {expected:?}"
    );
    Ok(())
}

fn assert_render_cases(cases: &[(u16, u16, &[u8], &[&str])]) -> Result<()> {
    for &(cols, rows, input, expected) in cases {
        let mut engine = TerminalEngine::new(test_geometry(cols, rows))?;
        engine.write_vt(input);
        assert_visible_text_rows(engine.extract_frame()?, expected)?;
    }
    Ok(())
}

type CursorRenderCase<'a> = (u16, u16, &'a [u8], &'a [&'a str], (u16, u16));

fn assert_cursor_render_cases(cases: &[CursorRenderCase<'_>]) -> Result<()> {
    for &(cols, rows, input, expected, cursor) in cases {
        let mut engine = TerminalEngine::new(test_geometry(cols, rows))?;
        engine.write_vt(input);
        let frame = engine.extract_frame()?;
        assert_visible_text_rows(frame, expected)?;
        assert_cursor_position(frame, cursor)?;
    }
    Ok(())
}

type ResizeCase<'a> = (
    u16,
    u16,
    &'a [u8],
    u16,
    u16,
    &'a [&'a str],
    Option<(u16, u16)>,
);

fn assert_resize_cases(cases: &[ResizeCase<'_>]) -> Result<()> {
    for &(cols, rows, input, resized_cols, resized_rows, expected, cursor) in cases {
        let mut engine = TerminalEngine::new(test_geometry(cols, rows))?;
        engine.write_vt(input);
        engine.resize(test_geometry(resized_cols, resized_rows))?;
        let frame = engine.extract_frame()?;
        assert_visible_text_rows(frame, expected)?;
        if let Some(cursor) = cursor {
            assert_cursor_position(frame, cursor)?;
        }
    }
    Ok(())
}

fn collect_visible_cells<T, F>(frame: &RenderFrame, map: F) -> Result<Vec<T>>
where
    F: Fn(char, &RenderCell) -> T,
{
    frame
        .cells
        .iter()
        .filter(|cell| cell.text_len > 0)
        .map(|cell| {
            Ok(map(
                *frame
                    .text
                    .get(cell.text_start)
                    .context("visible cell text")?,
                cell,
            ))
        })
        .collect()
}

fn rendered_palette_color(
    engine: &mut TerminalEngine,
    index: u8,
    marker: char,
) -> Result<Option<RgbColor>> {
    engine.write_vt(format!("\x1b[38;5;{index}m{marker}").as_bytes());
    let frame = engine.extract_frame()?;
    Ok(frame
        .cells
        .iter()
        .find(|cell| frame.cell_text(cell) == [marker])
        .and_then(|cell| cell.fg))
}

fn cursor_position(frame: &RenderFrame) -> Option<(u16, u16)> {
    frame.cursor.as_ref().map(|cursor| (cursor.x, cursor.y))
}

fn assert_cursor_position(frame: &RenderFrame, expected: (u16, u16)) -> Result<()> {
    anyhow::ensure!(
        cursor_position(frame) == Some(expected),
        "cursor {:?}, expected {expected:?}",
        cursor_position(frame)
    );
    Ok(())
}

#[test]
fn terminal_engine_applies_configured_default_colors_to_frame() {
    let mut engine = terminal_engine_with_colors(
        test_geometry(8, 2),
        TerminalColorConfig {
            background: rgb(0x10, 0x11, 0x12),
            foreground: rgb(0x20, 0x21, 0x22),
            cursor: Some(rgb(0x30, 0x31, 0x32)),
            cursor_text: Some(rgb(0x40, 0x41, 0x42)),
            pointer_foreground: None,
            pointer_background: None,
            tektronix_foreground: None,
            tektronix_background: None,
            highlight_background: None,
            tektronix_cursor: None,
            highlight_foreground: None,
            selection_background: Some(rgb(0x50, 0x51, 0x52)),
            selection_foreground: Some(rgb(0x60, 0x61, 0x62)),
            palette: vec![rgb(0, 1, 2), rgb(3, 4, 5)],
            palette_generate: false,
            palette_harmonious: false,
        },
    )
    .expect("test operation succeeds");

    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(frame.colors.background, rgb(0x10, 0x11, 0x12));
    assert_eq!(frame.colors.foreground, rgb(0x20, 0x21, 0x22));
    assert_eq!(frame.colors.cursor, Some(rgb(0x30, 0x31, 0x32)));
    assert_eq!(frame.colors.cursor_text, Some(rgb(0x40, 0x41, 0x42)));
    assert_eq!(
        frame.colors.selection_background,
        Some(rgb(0x50, 0x51, 0x52))
    );
    assert_eq!(
        frame.colors.selection_foreground,
        Some(rgb(0x60, 0x61, 0x62))
    );
    assert_eq!(
        rendered_palette_color(&mut engine, 0, 'A').expect("test operation succeeds"),
        Some(rgb(0, 1, 2))
    );
    assert_eq!(
        rendered_palette_color(&mut engine, 1, 'B').expect("test operation succeeds"),
        Some(rgb(3, 4, 5))
    );
}

#[test]
fn terminal_engine_updates_default_colors_live() {
    let mut engine = TerminalEngine::new(test_geometry(8, 2)).expect("test operation succeeds");

    engine
        .apply_live_config(TerminalLiveConfig {
            colors: TerminalColorConfig {
                background: RgbColor { r: 1, g: 2, b: 3 },
                foreground: RgbColor { r: 4, g: 5, b: 6 },
                cursor: Some(RgbColor { r: 7, g: 8, b: 9 }),
                cursor_text: Some(RgbColor {
                    r: 13,
                    g: 14,
                    b: 15,
                }),
                pointer_foreground: None,
                pointer_background: None,
                tektronix_foreground: None,
                tektronix_background: None,
                highlight_background: None,
                tektronix_cursor: None,
                highlight_foreground: None,
                selection_background: Some(RgbColor {
                    r: 16,
                    g: 17,
                    b: 18,
                }),
                selection_foreground: Some(RgbColor {
                    r: 19,
                    g: 20,
                    b: 21,
                }),
                palette: vec![RgbColor {
                    r: 10,
                    g: 11,
                    b: 12,
                }],
                palette_generate: false,
                palette_harmonious: false,
            },
            ..Default::default()
        })
        .expect("test operation succeeds");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(frame.colors.background, RgbColor { r: 1, g: 2, b: 3 });
    assert_eq!(frame.colors.foreground, RgbColor { r: 4, g: 5, b: 6 });
    assert_eq!(frame.colors.cursor, Some(RgbColor { r: 7, g: 8, b: 9 }));
    assert_eq!(
        frame.colors.cursor_text,
        Some(RgbColor {
            r: 13,
            g: 14,
            b: 15
        })
    );
    assert_eq!(
        rendered_palette_color(&mut engine, 0, 'A').expect("test operation succeeds"),
        Some(RgbColor {
            r: 10,
            g: 11,
            b: 12
        })
    );
}

#[test]
fn terminal_engine_applies_xterm_highlight_colors_to_frame_selection() {
    let mut engine = terminal_engine_with_colors(
        test_geometry(8, 2),
        TerminalColorConfig {
            highlight_background: Some(RgbColor {
                r: 0x20,
                g: 0x21,
                b: 0x22,
            }),
            highlight_foreground: Some(RgbColor {
                r: 0x30,
                g: 0x31,
                b: 0x32,
            }),
            selection_background: Some(RgbColor {
                r: 0x40,
                g: 0x41,
                b: 0x42,
            }),
            selection_foreground: Some(RgbColor {
                r: 0x50,
                g: 0x51,
                b: 0x52,
            }),
            ..Default::default()
        },
    )
    .expect("test operation succeeds");

    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(
        frame.colors.selection_background,
        Some(RgbColor {
            r: 0x20,
            g: 0x21,
            b: 0x22
        })
    );
    assert_eq!(
        frame.colors.selection_foreground,
        Some(RgbColor {
            r: 0x30,
            g: 0x31,
            b: 0x32
        })
    );

    engine.write_vt(b"\x1b]17;#123;#456;#abc\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(
        frame.colors.selection_background,
        Some(RgbColor {
            r: 0x11,
            g: 0x22,
            b: 0x33
        })
    );
    assert_eq!(
        frame.colors.selection_foreground,
        Some(RgbColor {
            r: 0xaa,
            g: 0xbb,
            b: 0xcc
        })
    );

    engine.write_vt(b"\x1b]117\x1b\\\x1b]119\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(
        frame.colors.selection_background,
        Some(RgbColor {
            r: 0x20,
            g: 0x21,
            b: 0x22
        })
    );
    assert_eq!(
        frame.colors.selection_foreground,
        Some(RgbColor {
            r: 0x30,
            g: 0x31,
            b: 0x32
        })
    );
}

fn assert_cursor_style(
    frame: &RenderFrame,
    expected_style: CursorVisualStyle,
    expected_blinking: bool,
) {
    assert_eq!(
        frame.cursor.map(|cursor| (cursor.style, cursor.blinking)),
        Some((expected_style, expected_blinking))
    );
}

proptest! {
    /// CUP always places output within the viewport: zero parameters mean the first row or
    /// column, and oversized parameters clamp to the final visible cell.
    #[test]
    fn absolute_cursor_position_clamps_to_viewport(case in any::<CursorCase>()) {
        let mut engine = TerminalEngine::new(test_geometry(case.cols, case.rows))
            .expect("terminal engine");

        engine.write_vt(format!("\x1b[{};{}HX", case.y, case.x).as_bytes());
        let frame = engine.extract_frame().expect("render frame");

        let expected_x = case.x.saturating_sub(1).min(case.cols.saturating_sub(1));
        let expected_y = case.y.saturating_sub(1).min(case.rows.saturating_sub(1));
        let expected_cursor_x = expected_x.saturating_add(1).min(case.cols.saturating_sub(1));
        prop_assert_eq!(occupied_cells(frame).expect("frame matches expected behavior"), vec![('X', expected_x, expected_y)]);
        prop_assert_eq!(cursor_position(frame), Some((expected_cursor_x, expected_y)));
    }

    /// Each relative cursor command moves along only its named axis, treats zero as one, and
    /// saturates at the corresponding viewport edge.
    #[test]
    fn relative_cursor_motion_clamps_to_viewport(case in any::<CursorCase>()) {
        let start_x = case.x.min(case.cols.saturating_sub(1));
        let start_y = case.y.min(case.rows.saturating_sub(1));
        let command = match case.direction {
            CursorDirection::Up => 'A',
            CursorDirection::Down => 'B',
            CursorDirection::Right => 'C',
            CursorDirection::Left => 'D',
        };
        let mut engine = TerminalEngine::new(test_geometry(case.cols, case.rows))
            .expect("terminal engine");

        engine.write_vt(
            format!("\x1b[{};{}H\x1b[{}{command}X", start_y.saturating_add(1), start_x.saturating_add(1), case.delta)
                .as_bytes(),
        );
        let frame = engine.extract_frame().expect("render frame");

        let effective_delta = case.delta.max(1);
        let expected_x = match case.direction {
            CursorDirection::Right => start_x.saturating_add(effective_delta).min(case.cols.saturating_sub(1)),
            CursorDirection::Left => start_x.saturating_sub(effective_delta),
            _ => start_x,
        };
        let expected_y = match case.direction {
            CursorDirection::Up => start_y.saturating_sub(effective_delta),
            CursorDirection::Down => start_y.saturating_add(effective_delta).min(case.rows.saturating_sub(1)),
            _ => start_y,
        };
        let expected_cursor_x = expected_x.saturating_add(1).min(case.cols.saturating_sub(1));
        prop_assert_eq!(occupied_cells(frame).expect("frame matches expected behavior"), vec![('X', expected_x, expected_y)]);
        prop_assert_eq!(cursor_position(frame), Some((expected_cursor_x, expected_y)));
    }

    /// A default tab advances to the next eight-column stop, or the right edge when the next stop
    /// falls outside the viewport.
    #[test]
    fn terminal_default_tab_movement_lands_on_next_tabstop_or_right_edge(
        case in any::<CursorCase>(),
    ) {
        let start_x = case.x.min(case.cols.saturating_sub(1));
        let mut engine = TerminalEngine::new(test_geometry(case.cols, 2)).expect("terminal engine");

        engine.write_vt(format!("\x1b[{}G\tX", start_x.saturating_add(1)).as_bytes());
        let frame = engine.extract_frame().expect("render frame");

        let expected_x = (start_x / 8).saturating_add(1).saturating_mul(8).min(case.cols.saturating_sub(1));
        let expected_cursor_x = expected_x.saturating_add(1).min(case.cols.saturating_sub(1));
        prop_assert_eq!(occupied_cells(frame).expect("frame matches expected behavior"), vec![('X', expected_x, 0)]);
        prop_assert_eq!(cursor_position(frame), Some((expected_cursor_x, 0)));
    }
}

#[test]
fn terminal_engine_supports_tabstop_set_after_clear() {
    let mut engine = TerminalEngine::new(test_geometry(16, 2)).expect("test operation succeeds");

    engine.write_vt(b"\x1b[3g\x1b[5G\x1bH\rA\tB");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        occupied_cells(frame).expect("frame matches expected behavior"),
        [('A', 0, 0), ('B', 4, 0)]
    );
}

#[test]
fn terminal_engine_supports_large_column_custom_tabstop() {
    let mut engine = TerminalEngine::new(test_geometry(600, 1)).expect("test operation succeeds");

    engine.write_vt(b"\x1b[3g\x1b[519G\x1bH\x1b[1GA\tB");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        occupied_cells(frame).expect("frame matches expected behavior"),
        [('A', 0, 0), ('B', 518, 0)]
    );
}

#[test]
fn terminal_engine_decodes_well_formed_utf8_bytes() {
    let mut engine = TerminalEngine::new(test_geometry(16, 1)).expect("test operation succeeds");

    engine.write_vt("😄✤ÁA".as_bytes());
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        occupied_cells(frame).expect("frame matches expected behavior"),
        [('😄', 0, 0), ('✤', 2, 0), ('Á', 3, 0), ('A', 4, 0)]
    );
}

#[test]
fn terminal_engine_replaces_partially_invalid_utf8_bytes() {
    let mut engine = TerminalEngine::new(test_geometry(16, 1)).expect("test operation succeeds");

    engine.write_vt(b"\xF0\x9F");
    engine.write_vt("😄".as_bytes());
    engine.write_vt(b"\xED\xA0\x80");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        occupied_cells(frame).expect("frame matches expected behavior"),
        [
            ('\u{FFFD}', 0, 0),
            ('😄', 1, 0),
            ('\u{FFFD}', 3, 0),
            ('\u{FFFD}', 4, 0),
            ('\u{FFFD}', 5, 0),
        ]
    );
}

#[test]
fn terminal_engine_decodes_text_and_character_sets() {
    assert_render_cases(&[
        (16, 1, b"Hello, World!", &["Hello, World!"]),
        (16, 1, "\x1b(A#\x1b(B#\x1b(0`qx😄".as_bytes(), &["£#◆─│ "]),
        (16, 1, b"`\x1b)0\x0e``\x0f`\x1b*0\x1bN``", &["`◆◆`◆`"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_kitty_color_protocol_specials() {
    let mut engine = TerminalEngine::new(test_geometry(8, 1)).expect("test operation succeeds");

    engine.write_vt(
        b"\x1b]21;foreground=rgb:12/34/56;background=rgb:78/9a/bc;cursor=rgb:de/f0/12\x1b\\X",
    );
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        frame.colors.foreground,
        RgbColor {
            r: 0x12,
            g: 0x34,
            b: 0x56
        }
    );
    assert_eq!(
        frame.colors.background,
        RgbColor {
            r: 0x78,
            g: 0x9a,
            b: 0xbc
        }
    );
    assert_eq!(
        frame.colors.cursor,
        Some(RgbColor {
            r: 0xde,
            g: 0xf0,
            b: 0x12
        })
    );
}

#[test]
fn terminal_engine_supports_kitty_color_protocol_palette_set_and_reset() {
    let mut engine = TerminalEngine::new(test_geometry(8, 1)).expect("test operation succeeds");

    let magenta = RgbColor {
        r: 0xff,
        g: 0x00,
        b: 0xff,
    };
    engine.write_vt(b"\x1b]21;5=rgb:ff/00/ff\x1b\\");

    engine.write_vt(b"\x1b[35mMM");
    let frame = engine.extract_frame().expect("test operation succeeds");
    let colored_cells =
        collect_visible_cells(frame, |text, cell| (text, cell.fg)).expect("visible cells");

    assert_eq!(colored_cells.len(), 2);
    assert_eq!(
        colored_cells[0],
        ('M', Some(magenta)),
        "expected kitty OSC 21 numeric palette key to update SGR palette color"
    );

    engine.write_vt(b"\x1b]21;5=\x1b\\D");
    let frame = engine.extract_frame().expect("test operation succeeds");
    let reset_cells =
        collect_visible_cells(frame, |text, cell| (text, cell.fg)).expect("visible cells");
    let reset_cell = reset_cells
        .iter()
        .find(|(text, _)| *text == 'D')
        .context("reset marker cell should be visible")
        .expect("test operation succeeds");
    assert_ne!(reset_cell.1, Some(magenta));
}

#[test]
fn terminal_engine_regenerates_palette_from_pristine_base_on_color_reload() {
    let mut pristine = TerminalEngine::new(test_geometry(8, 1)).expect("test operation succeeds");
    let pristine_index_17 =
        rendered_palette_color(&mut pristine, 17, 'P').expect("test operation succeeds");
    let mut colors = TerminalColorConfig {
        palette_generate: true,
        ..Default::default()
    };
    let mut engine = terminal_engine_with_colors(test_geometry(8, 1), colors.clone())
        .expect("test operation succeeds");

    assert_ne!(
        rendered_palette_color(&mut engine, 17, 'G').expect("test operation succeeds"),
        pristine_index_17
    );

    colors.palette_generate = false;
    engine
        .apply_live_config(TerminalLiveConfig {
            colors,
            ..Default::default()
        })
        .expect("test operation succeeds");

    assert_eq!(
        rendered_palette_color(&mut engine, 17, 'R').expect("test operation succeeds"),
        pristine_index_17
    );
}

#[test]
fn terminal_engine_generates_palette_from_color_config() {
    let colors = TerminalColorConfig {
        background: rgb(0x1e, 0x1e, 0x2e),
        foreground: rgb(0xcd, 0xd6, 0xf4),
        palette: vec![
            rgb(0x45, 0x45, 0x5a),
            rgb(0xf3, 0x8b, 0xa8),
            rgb(0xa6, 0xe3, 0xa1),
            rgb(0xf9, 0xe2, 0xaf),
            rgb(0x89, 0xb4, 0xfa),
            rgb(0xf5, 0xc2, 0xe7),
            rgb(0x94, 0xe2, 0xd5),
            rgb(0xba, 0xc2, 0xde),
            rgb(0x58, 0x5b, 0x70),
            rgb(0xf3, 0x8b, 0xa8),
            rgb(0xa6, 0xe3, 0xa1),
            rgb(0xf9, 0xe2, 0xaf),
            rgb(0x89, 0xb4, 0xfa),
            rgb(0xf5, 0xc2, 0xe7),
            rgb(0x94, 0xe2, 0xd5),
            rgb(0xa6, 0xad, 0xcb),
        ],
        palette_generate: true,
        ..Default::default()
    };
    let mut engine =
        terminal_engine_with_colors(test_geometry(2, 1), colors).expect("test operation succeeds");

    engine.write_vt(b"\x1b[38;5;17mG");
    let frame = engine.extract_frame().expect("test operation succeeds");
    let colored_cells =
        collect_visible_cells(frame, |text, cell| (text, cell.fg)).expect("visible cells");

    assert_eq!(colored_cells, [('G', Some(rgb(0x32, 0x38, 0x52)))]);
}

#[test]
fn terminal_engine_supports_osc_color_operations() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    let red = RgbColor {
        r: 0xff,
        g: 0x00,
        b: 0x00,
    };
    let green = RgbColor {
        r: 0x00,
        g: 0xff,
        b: 0x00,
    };
    let blue = RgbColor {
        r: 0x00,
        g: 0x00,
        b: 0xff,
    };

    engine.write_vt(b"\x1b]4;42;rgb:ff/00/00;43;rgb:00/ff/00\x1b\\");
    engine.write_vt(b"\x1b[38;5;42mR\x1b[38;5;43mG");
    let frame = engine.extract_frame().expect("test operation succeeds");
    let colored_cells =
        collect_visible_cells(frame, |text, cell| (text, cell.fg)).expect("visible cells");
    assert!(colored_cells.contains(&('R', Some(red))));
    assert!(colored_cells.contains(&('G', Some(green))));

    engine.write_vt(b"\x1b]104;42;;43\x1b\\");
    engine.write_vt(b"\x1b[38;5;42mD\x1b[38;5;43mE");
    let reset_frame = engine.extract_frame().expect("test operation succeeds");
    let reset_cells =
        collect_visible_cells(reset_frame, |text, cell| (text, cell.fg)).expect("visible cells");
    assert!(reset_cells.contains(&('D', None)) || !reset_cells.contains(&('D', Some(red))));
    assert!(reset_cells.contains(&('E', None)) || !reset_cells.contains(&('E', Some(green))));

    engine.write_vt(b"\x1b]10;rgb:ff/00/00;rgb:00/00/ff\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(frame.colors.foreground, red);
    assert_eq!(frame.colors.background, blue);

    engine.write_vt(b"\x1b]12;rgb:00/ff/00\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(frame.colors.cursor, Some(green));

    engine.write_vt(b"\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_ne!(frame.colors.foreground, red);
    assert_ne!(frame.colors.background, blue);
    assert_ne!(frame.colors.cursor, Some(green));
}

#[test]
fn terminal_engine_supports_x11_color_names_in_color_operations() {
    let mut engine = TerminalEngine::new(test_geometry(12, 1)).expect("test operation succeeds");

    let white = RgbColor {
        r: 255,
        g: 255,
        b: 255,
    };
    let forest_green = RgbColor {
        r: 34,
        g: 139,
        b: 34,
    };
    let medium_spring_green = RgbColor {
        r: 0,
        g: 250,
        b: 154,
    };
    let lawn_green = RgbColor {
        r: 124,
        g: 252,
        b: 0,
    };
    let black = RgbColor { r: 0, g: 0, b: 0 };

    engine.write_vt(
        b"\x1b]4;1;red;2;green;4;blue;7;white;42;FoReStGReen;43;mediumspringgreen;44;black\x1b\\",
    );

    engine.write_vt(b"\x1b]4;45;rgbi:1.0/0/0;46;rgb:7f/a0a0/0;47;rgb:f/ff/fff;48;#fff;49;#fffffffff;50;#ffffffffffff;51;#ff0010\x1b\\");

    engine.write_vt(b"\x1b[38;5;42mF\x1b[38;5;43mM\x1b[38;5;44mK");
    let frame = engine.extract_frame().expect("test operation succeeds");
    let colored_cells =
        collect_visible_cells(frame, |text, cell| (text, cell.fg)).expect("visible cells");
    assert!(colored_cells.contains(&('F', Some(forest_green))));
    assert!(colored_cells.contains(&('M', Some(medium_spring_green))));
    assert!(colored_cells.contains(&('K', Some(black))));

    engine.write_vt(b"\x1b]10;medium spring green;ForestGreen\x1b\\\x1b]12;lawngreen\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(frame.colors.foreground, medium_spring_green);
    assert_eq!(frame.colors.background, forest_green);
    assert_eq!(frame.colors.cursor, Some(lawn_green));

    engine.write_vt(b"\x1b]21;foreground= Forest Green ;background=LawnGreen;cursor=white\x1b\\");
    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!(frame.colors.foreground, forest_green);
    assert_eq!(frame.colors.background, lawn_green);
    assert_eq!(frame.colors.cursor, Some(white));

    engine.write_vt(b"\x1b]4;42;nosuchcolor\x1b\\");
    engine.write_vt(b"\x1b]4;51;rgb:not/hex/zz\x1b\\");
    engine.write_vt(b"\x1b[38;5;42mN\x1b[38;5;51mI");
    let invalid_frame = engine.extract_frame().expect("test operation succeeds");
    let invalid_cells =
        collect_visible_cells(invalid_frame, |text, cell| (text, cell.fg)).expect("visible cells");
    assert!(invalid_cells.contains(&('N', Some(forest_green))));
    assert!(invalid_cells.contains(&(
        'I',
        Some(RgbColor {
            r: 255,
            g: 0,
            b: 16
        })
    )));

    engine.write_vt(b"\x1b]104;42;43;44;45;46;47;48;49;50;51\x1b\\");
}

#[test]
fn terminal_engine_supports_screen_style_state_and_reset() {
    let mut engine = TerminalEngine::new(test_geometry(8, 2)).expect("test operation succeeds");

    engine.write_vt(b"\x1b[1mB\x1b[22mN\x1b[3mI\x1b[0mP");
    let frame = engine.extract_frame().expect("test operation succeeds");
    let styled_cells = collect_visible_cells(frame, |text, cell| {
        (text, cell.style.bold, cell.style.italic)
    })
    .expect("visible cells");

    assert_eq!(
        styled_cells,
        [
            ('B', true, false),
            ('N', false, false),
            ('I', false, true),
            ('P', false, false),
        ]
    );
}

#[test]
fn terminal_engine_supports_terminal_cursor_position_edges() {
    assert_render_cases(&[
        (5, 5, b"\x1b[3;4r\x1b[?6h\x1b[1;1HX", &["", "", "X"]),
        (
            5,
            5,
            b"\x1b[3;4r\x1b[?6h\x1b[500;500HX",
            &["", "", "", "    X"],
        ),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_cursor_line_and_axis_controls() {
    assert_render_cases(&[
        (5, 5, b"\x1b[3GX", &["  X"]),
        (5, 5, b"\x1b[3dX", &["", "", "X"]),
        (5, 5, b"A\x1b[2aX", &["A  X"]),
        (5, 5, b"A\x1b[2eX", &["A", "", " X"]),
        (5, 5, b"\x1b[3;4HB\x1b[EX", &["", "", "   B", "X"]),
        (5, 5, b"\x1b[3;4HB\x1b[FX", &["", "X", "   B"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_margin_setting_controls() {
    assert_render_cases(&[
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[2r\x1b[T",
            &["ABC", "", "DEF", "GHI"],
        ),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[1;2r\x1b[T",
            &["", "ABC", "GHI"],
        ),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[?69h\x1b[2s\x1b[1;2H\x1b[L",
            &["A", "DBC", "GEF", " HI"],
        ),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[?69h\x1b[1;2s\x1b[1;2H\x1b[L",
            &["  C", "ABF", "DEI", "GH"],
        ),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[1;2s\x1b[1;2H\x1b[L",
            &["", "ABC", "DEF", "GHI"],
        ),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_cursor_style_controls() {
    let mut engine = TerminalEngine::new(test_geometry(5, 5)).expect("test operation succeeds");

    for (command, style, blinking) in [
        (b"\x1b[1 q".as_ref(), CursorVisualStyle::Block, true),
        (b"\x1b[2 q".as_ref(), CursorVisualStyle::Block, false),
        (b"\x1b[3 q".as_ref(), CursorVisualStyle::Underline, true),
        (b"\x1b[5 q".as_ref(), CursorVisualStyle::Bar, true),
        (b"\x1b[q".as_ref(), CursorVisualStyle::Bar, true),
    ] {
        engine.write_vt(command);
        let frame = engine.extract_frame().expect("test operation succeeds");
        assert_cursor_style(frame, style, blinking);
    }
}

#[test]
fn terminal_engine_supports_grapheme_width_and_wrap() {
    assert_cursor_render_cases(&[
        (
            4,
            5,
            "\x1b[?2027h🍋☔\u{fe0e}".as_bytes(),
            &["🍋☔\u{fe0e}"],
            (3, 0),
        ),
        (
            3,
            5,
            "\x1b[?2027h#\x1b[3G#\u{fe0f}".as_bytes(),
            &["#", "#\u{fe0f}"],
            (2, 1),
        ),
        (
            3,
            5,
            "\x1b[?2027h\x1b[2G#\u{fe0f}".as_bytes(),
            &[" #\u{fe0f}"],
            (2, 0),
        ),
        (
            3,
            5,
            "\x1b[?2027h\x1b[3G\u{0915}\u{094d}\u{200d}\u{0937}".as_bytes(),
            &["", "क\u{094d}\u{200d}ष"],
            (2, 1),
        ),
        (8, 2, "\u{1F600}".as_bytes(), &["\u{1F600}"], (2, 0)),
        (
            10,
            3,
            "\x1b[1;10H\u{1F600}".as_bytes(),
            &["", "\u{1F600}"],
            (2, 1),
        ),
        (8, 2, "\u{200D}".as_bytes(), &[], (0, 0)),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_cursor_movement() {
    assert_render_cases(&[
        (5, 5, b"\x1b[2;4r\x1b[3;1HA\x1b[5AX", &["", " X", "A"]),
        (5, 5, b"\x1b[1;3rA\x1b[10BX", &["A", "", " X"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_control_chars_and_tabs() {
    assert_render_cases(&[
        (10, 5, b"hello\r\nworld", &["hello", "world"]),
        (5, 5, b"hello\nX", &["hello", "    X"]),
        (10, 5, b"\x1b[20h123456\nX", &["123456", "X"]),
        (5, 5, b"hello\rX", &["Xello"]),
        (10, 5, b"hello\x08y", &["helly"]),
        (20, 5, b"\x1b[20G\x1b[ZB\x1b[2ZC", &["        C       B"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_index_and_reverse_index() {
    assert_render_cases(&[
        (5, 5, b"\x1bDX", &["", "X"]),
        (5, 5, b"\x1bEX", &["", "X"]),
        (5, 5, b"\x1b[5;1HA\x1b[D\x1bDX", &["", "", "", "A", "X"]),
        (
            5,
            5,
            b"A\x1b[2;1HB\x1b[3;1HC\x1b[1;1H\x1bMX",
            &["X", "A", "B", "C"],
        ),
        (
            5,
            5,
            b"A\x1b[2;1HB\x1b[3;1HC\x1b[2;1H\x1bMX",
            &["X", "B", "C"],
        ),
        (
            5,
            5,
            b"A\x1b[2;1HB\x1b[3;1HC\x1b[2;3r\x1b[2;1H\x1bM",
            &["A", "", "B"],
        ),
        (
            5,
            5,
            b"A\x1b[2;1HB\x1b[3;1HC\x1b[2;3r\x1b[1;1H\x1bM",
            &["A", "B", "C"],
        ),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_scroll_up_and_down() {
    assert_render_cases(&[
        (5, 5, b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[S", &["DEF", "GHI"]),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[T",
            &["", "ABC", "DEF", "GHI"],
        ),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[2;3r\x1b[1;1H\x1b[S",
            &["ABC", "GHI"],
        ),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[3;4r\x1b[2;2H\x1b[T",
            &["ABC", "DEF", "", "GHI"],
        ),
        (
            5,
            5,
            b"AAAAA\r\nBBBBB\r\nCCCCC\r\nDDDDD\x1b[2S",
            &["CCCCC", "DDDDD"],
        ),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_tab_clear_set_and_reset() {
    assert_render_cases(&[
        (30, 5, b"\t\x1b[2W\x1b[1;1HA\tX", &["A               X"]),
        (
            30,
            5,
            b"\x1b[5W\x1b[1;1HA\tX",
            &["A                            X"],
        ),
        (30, 5, b"\x1b[5W\x1b[5G\x1b[W\x1b[1;1HA\tX", &["A   X"]),
        (
            30,
            5,
            b"\x1b[5W\x1b[5G\x1b[W\x1b[?5W\x1b[1;1HA\tX",
            &["A       X"],
        ),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_print_repeat() {
    assert_render_cases(&[
        (5, 5, b"A\x1b[b", &["AA"]),
        (5, 5, b"A\x1b[2b", &["AAA"]),
        (5, 5, b"    A\x1b[b", &["    A", "A"]),
        (5, 5, b"\x1b[b", &[]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_alternate_screen_modes() {
    let mut mode_47 = TerminalEngine::new(test_geometry(5, 5)).expect("test operation succeeds");
    mode_47.write_vt(b"1A\x1b[?47h");
    assert_visible_text_rows(
        mode_47.extract_frame().expect("test operation succeeds"),
        &[],
    )
    .expect("frame matches expected behavior");
    mode_47.write_vt(b"2B");
    assert_visible_text_rows(
        mode_47.extract_frame().expect("test operation succeeds"),
        &["  2B"],
    )
    .expect("frame matches expected behavior");
    mode_47.write_vt(b"\x1b[?47l");
    assert_visible_text_rows(
        mode_47.extract_frame().expect("test operation succeeds"),
        &["1A"],
    )
    .expect("frame matches expected behavior");
    mode_47.write_vt(b"\x1b[?47h");
    assert_visible_text_rows(
        mode_47.extract_frame().expect("test operation succeeds"),
        &["  2B"],
    )
    .expect("frame matches expected behavior");

    let mut mode_1047 = TerminalEngine::new(test_geometry(5, 5)).expect("test operation succeeds");
    mode_1047.write_vt(b"1A\x1b[?1047h");
    assert_visible_text_rows(
        mode_1047.extract_frame().expect("test operation succeeds"),
        &[],
    )
    .expect("frame matches expected behavior");
    mode_1047.write_vt(b"2B");
    assert_visible_text_rows(
        mode_1047.extract_frame().expect("test operation succeeds"),
        &["  2B"],
    )
    .expect("frame matches expected behavior");
    mode_1047.write_vt(b"\x1b[?1047l");
    assert_visible_text_rows(
        mode_1047.extract_frame().expect("test operation succeeds"),
        &["1A"],
    )
    .expect("frame matches expected behavior");
    mode_1047.write_vt(b"\x1b[?1047h");
    assert_visible_text_rows(
        mode_1047.extract_frame().expect("test operation succeeds"),
        &[],
    )
    .expect("frame matches expected behavior");

    let mut mode_1049 = TerminalEngine::new(test_geometry(5, 5)).expect("test operation succeeds");
    mode_1049.write_vt(b"1A\x1b[?1049h");
    assert_visible_text_rows(
        mode_1049.extract_frame().expect("test operation succeeds"),
        &[],
    )
    .expect("frame matches expected behavior");
    mode_1049.write_vt(b"2B");
    assert_visible_text_rows(
        mode_1049.extract_frame().expect("test operation succeeds"),
        &["  2B"],
    )
    .expect("frame matches expected behavior");
    mode_1049.write_vt(b"\x1b[?1049lC");
    assert_visible_text_rows(
        mode_1049.extract_frame().expect("test operation succeeds"),
        &["1AC"],
    )
    .expect("frame matches expected behavior");
    mode_1049.write_vt(b"\x1b[?1049h");
    assert_visible_text_rows(
        mode_1049.extract_frame().expect("test operation succeeds"),
        &[],
    )
    .expect("frame matches expected behavior");
}

#[test]
fn terminal_engine_supports_terminal_full_reset() {
    let mut origin_mode =
        TerminalEngine::new(test_geometry(10, 10)).expect("test operation succeeds");
    origin_mode.write_vt(b"\x1b[3;4r\x1b[?6h\x1b[1;1HA\x1bcX");
    assert_visible_text_rows(
        origin_mode
            .extract_frame()
            .expect("test operation succeeds"),
        &["X"],
    )
    .expect("frame matches expected behavior");

    let mut saved_cursor =
        TerminalEngine::new(test_geometry(10, 10)).expect("test operation succeeds");
    saved_cursor.write_vt(b"\x1b[3;5H\x1b7\x1bc\x1b8X");
    let frame = saved_cursor
        .extract_frame()
        .expect("test operation succeeds");
    assert_visible_text_rows(frame, &["X"]).expect("frame matches expected behavior");
    assert_cursor_position(frame, (1, 0)).expect("frame matches expected behavior");

    let mut alternate =
        TerminalEngine::new(test_geometry(10, 10)).expect("test operation succeeds");
    alternate.write_vt(b"primary\x1b[?1049halt\x1b[?1049l\x1bc");
    assert_visible_text_rows(
        alternate.extract_frame().expect("test operation succeeds"),
        &[],
    )
    .expect("frame matches expected behavior");
    alternate.write_vt(b"\x1b[?1049h");
    assert_visible_text_rows(
        alternate.extract_frame().expect("test operation succeeds"),
        &[],
    )
    .expect("frame matches expected behavior");

    let mut style_reset =
        TerminalEngine::new(test_geometry(10, 10)).expect("test operation succeeds");
    style_reset.write_vt(b"\x1b[1;3mA\x1bcX");
    let frame = style_reset
        .extract_frame()
        .expect("test operation succeeds");
    let cell = frame
        .cells
        .iter()
        .find(|cell| cell.text_len > 0)
        .expect("reset should leave one printed cell");
    assert_eq!(frame.cell_text(cell), &['X']);
    assert!(!cell.style.bold);
    assert!(!cell.style.italic);
}

#[test]
fn terminal_engine_supports_terminal_plain_text_input() {
    let mut engine = TerminalEngine::new(test_geometry(40, 4)).expect("test operation succeeds");

    engine.write_vt(b"hello");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_visible_text_rows(frame, &["hello"]).expect("frame matches expected behavior");
    assert_cursor_position(frame, (5, 0)).expect("frame matches expected behavior");
    assert_eq!(frame.row_dirty.first().copied(), Some(true));
}

#[test]
fn terminal_engine_supports_terminal_basic_wraparound_printing() {
    let mut engine = TerminalEngine::new(test_geometry(5, 4)).expect("test operation succeeds");

    engine.write_vt(b"helloworldabc12");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_visible_text_rows(frame, &["hello", "world", "abc12"])
        .expect("frame matches expected behavior");
    assert_cursor_position(frame, (4, 2)).expect("frame matches expected behavior");
}

#[test]
fn terminal_engine_supports_terminal_input_forces_scroll() {
    let mut engine = TerminalEngine::new(test_geometry(1, 5)).expect("test operation succeeds");

    engine.write_vt(b"abcdef");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_visible_text_rows(frame, &["b", "c", "d", "e", "f"])
        .expect("frame matches expected behavior");
    assert_cursor_position(frame, (0, 4)).expect("frame matches expected behavior");
}

#[test]
fn terminal_engine_supports_terminal_single_very_long_line() {
    let mut engine = TerminalEngine::new(test_geometry(5, 5)).expect("test operation succeeds");

    engine.write_vt(&vec![b'x'; 1000]);
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(frame.rows, 5);
    assert_eq!(frame.cols, 5);
    assert_cursor_position(frame, (4, 4)).expect("frame matches expected behavior");
    assert_eq!(
        visible_text_rows(frame).expect("frame matches expected behavior"),
        vec!["xxxxx"; 5]
    );
}

#[test]
fn terminal_engine_supports_terminal_unique_style_per_cell() {
    let mut engine = TerminalEngine::new(test_geometry(30, 30)).expect("test operation succeeds");

    for y in 0..30 {
        for x in 0..30 {
            engine
                .write_vt(format!("\x1b[{};{}H\x1b[48;2;{};{};0mx", y + 1, x + 1, x, y).as_bytes());
        }
    }
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        collect_visible_cells(frame, |text, _| text)
            .expect("visible cells")
            .len(),
        900
    );
    for (x, y) in [(0, 0), (7, 3), (29, 29)] {
        let cell = frame
            .cells
            .iter()
            .find(|cell| cell.x == x && cell.y == y)
            .unwrap_or_else(|| panic!("missing styled cell at {x},{y}"));
        assert_eq!(frame.cell_text(cell), &['x']);
        assert_eq!(
            cell.bg,
            Some(RgbColor {
                r: u8::try_from(x).expect("red channel"),
                g: u8::try_from(y).expect("green channel"),
                b: 0
            })
        );
    }
}

#[test]
fn terminal_engine_supports_terminal_resize_reflow_visible_content() {
    assert_resize_cases(&[
        (4, 2, b"0123", 2, 2, &["01", "23"], None),
        (4, 2, b"\x1b[?7l0123", 2, 2, &["01"], None),
        (
            10,
            3,
            b"1ABCD\r\n2EFGH\r\n3IJKL",
            10,
            10,
            &["1ABCD", "2EFGH", "3IJKL"],
            None,
        ),
        (
            10,
            3,
            b"1ABCD\r\n2EFGH\r\n3IJKL",
            10,
            2,
            &["2EFGH", "3IJKL"],
            Some((5, 1)),
        ),
        (
            10,
            3,
            b"1ABCD\r\n2EFGH\r\n3IJKL",
            20,
            3,
            &["1ABCD", "2EFGH", "3IJKL"],
            None,
        ),
        (5, 3, b"1ABCD", 3, 3, &["1AB", "CD"], None),
        (5, 3, b"\x1b[?1049h1ABCD", 3, 3, &["1AB"], Some((2, 0))),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_resizes_main_screen_after_many_lines_without_overflow() {
    let mut engine = TerminalEngine::new(test_geometry(120, 40)).expect("test operation succeeds");
    let mut payload = Vec::new();
    for index in 0..24 {
        payload.extend_from_slice(
            format!("normal line {index:06} {}\r\n", "payload ".repeat(6)).as_bytes(),
        );
    }
    engine.write_vt(&payload);

    engine
        .resize(test_geometry(80, 24))
        .expect("test operation succeeds");

    let frame = engine.extract_frame().expect("test operation succeeds");
    assert_eq!((frame.cols, frame.rows), (80, 24));
}

#[test]
fn terminal_engine_supports_screen_clear_active_line() {
    let mut engine = TerminalEngine::new(test_geometry(8, 2)).expect("test operation succeeds");

    engine.write_vt(b"hello\r\x1b[K");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        occupied_cells(frame).expect("frame matches expected behavior"),
        Vec::<(char, u16, u16)>::new()
    );
}

#[test]
fn terminal_engine_supports_terminal_erase_chars() {
    assert_render_cases(&[
        (5, 5, b"ABC\x1b[1;1H\x1b[2XX", &["X C"]),
        (5, 5, b"ABC\x1b[1;1H\x1b[0XX", &["XBC"]),
        (5, 5, b"  ABC\x1b[1;4H\x1b[10X", &["  A"]),
        (5, 5, b"ABCDE\x1b[XB", &["ABCDB"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_erase_line() {
    assert_render_cases(&[
        (5, 5, b"ABCDE\x1b[1;3H\x1b[K", &["AB"]),
        (5, 5, b"ABCDE\x1b[KB", &["ABCDB"]),
        (5, 5, b"ABCDE123\x1b[1;1H\x1b[KX", &["X", "123"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_insert_blanks() {
    assert_render_cases(&[
        (5, 2, b"ABC\x1b[1;1H\x1b[2@", &["  ABC"]),
        (3, 2, b"ABC\x1b[1;1H\x1b[2@", &["  A"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_insert_mode_printing() {
    assert_render_cases(&[
        (10, 2, b"hello\x1b[1;2H\x1b[4hX", &["hXello"]),
        (5, 2, b"hello\x1b[1;2H\x1b[4hX", &["hXell"]),
        (5, 2, b"hello\x1b[4hX", &["hello", "X"]),
        (
            5,
            2,
            "hello\x1b[1;2H\x1b[4h\u{1F600}".as_bytes(),
            &["h\u{1F600}el"],
        ),
        (5, 2, "123\u{1F600}\x1b[1;1H\x1b[4hX".as_bytes(), &["X123"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_delete_chars() {
    assert_render_cases(&[
        (5, 5, b"ABCDE\x1b[1;2H\x1b[2P", &["ADE"]),
        (5, 5, b"ABCDE\x1b[1;2H\x1b[10P", &["A"]),
        (5, 5, b"ABCDE\x1b[PX", &["ABCDX"]),
        (5, 5, b"ABCDE123\x1b[1;1H\x1b[PX", &["XCDE", "123"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_insert_lines() {
    assert_render_cases(&[
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[L",
            &["ABC", "", "DEF", "GHI"],
        ),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\r\n123\x1b[1;3r\x1b[2;2H\x1b[L",
            &["ABC", "", "DEF", "123"],
        ),
        (2, 5, b"A\r\nB\r\nC\r\nD\r\nE\x1b[2;1H\x1b[20L", &["A"]),
        (5, 5, b"ABCDE\x1b[LB", &["B", "ABCDE"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_delete_lines() {
    assert_render_cases(&[
        (5, 5, b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[M", &["ABC", "GHI"]),
        (
            5,
            5,
            b"A\r\nB\r\nC\r\nD\x1b[1;3r\x1b[1;1H\x1b[ME\r\n",
            &["E", "C", "", "D"],
        ),
        (
            5,
            5,
            b"A\r\nB\r\nC\r\nD\x1b[1;3r\x1b[1;1H\x1b[5ME\r\n",
            &["E", "", "", "D"],
        ),
        (5, 5, b"ABCDE\x1b[MB", &["B"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_erase_display() {
    assert_render_cases(&[
        (5, 5, b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[J", &["ABC", "D"]),
        (
            5,
            5,
            b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[1J",
            &["", "  F", "GHI"],
        ),
    ])
    .expect("test operation succeeds");

    let mut complete = TerminalEngine::new(test_geometry(5, 5)).expect("test operation succeeds");
    complete.write_vt(b"ABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[2J");
    let complete_frame = complete.extract_frame().expect("test operation succeeds");
    assert_eq!(
        occupied_cells(complete_frame).expect("frame matches expected behavior"),
        Vec::<(char, u16, u16)>::new()
    );
    assert_cursor_position(complete_frame, (1, 1)).expect("frame matches expected behavior");
}

#[test]
fn terminal_engine_supports_terminal_save_restore_cursor() {
    assert_render_cases(&[
        (10, 5, b"\x1b[1;5HA\x1b7\x1b[1;1HB\x1b8X", &["B   AX"]),
        (5, 5, b"\x1b[1;5HA\x1b7\x1b[1;1HB\x1b8X", &["B   A", "X"]),
    ])
    .expect("test operation succeeds");

    let mut resized = TerminalEngine::new(test_geometry(10, 5)).expect("test operation succeeds");
    resized.write_vt(b"\x1b[1;10H\x1b7");
    resized
        .resize(test_geometry(5, 5))
        .expect("test operation succeeds");
    resized.write_vt(b"\x1b8X");
    assert_visible_text_rows(
        resized.extract_frame().expect("test operation succeeds"),
        &["    X"],
    )
    .expect("frame matches expected behavior");

    let mut style = TerminalEngine::new(test_geometry(5, 2)).expect("test operation succeeds");
    style.write_vt(b"\x1b[1m\x1b7\x1b[22mn\x1b8b");
    let frame = style.extract_frame().expect("test operation succeeds");
    let styled_cells =
        collect_visible_cells(frame, |text, cell| (text, cell.style.bold)).expect("visible cells");
    assert_eq!(styled_cells, [('b', true)]);
}

#[test]
fn terminal_engine_supports_terminal_protected_erase() {
    assert_render_cases(&[
        (
            5,
            5,
            b"\x1bVABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[J",
            &["ABC", "DEF", "GHI"],
        ),
        (
            5,
            5,
            b"\x1b[1\"qABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[J",
            &["ABC", "D"],
        ),
        (
            5,
            5,
            b"\x1b[1\"qABC\r\nDEF\r\nGHI\x1b[2;2H\x1b[?J",
            &["ABC", "DEF", "GHI"],
        ),
        (5, 2, b"\x1b[1\"qABCDE\x1b[1;3H\x1b[?K", &["ABCDE"]),
    ])
    .expect("terminal behavior matches cases");
}

#[test]
fn terminal_engine_supports_terminal_decaln() {
    let mut simple = TerminalEngine::new(test_geometry(2, 2)).expect("test operation succeeds");
    simple.write_vt(b"A\r\nB\x1b#8");
    let frame = simple.extract_frame().expect("test operation succeeds");
    assert_visible_text_rows(frame, &["EE", "EE"]).expect("frame matches expected behavior");
    assert_cursor_position(frame, (0, 0)).expect("frame matches expected behavior");
    assert_eq!(frame.row_dirty, [true, true]);

    let mut color = TerminalEngine::new(test_geometry(3, 3)).expect("test operation succeeds");
    color.write_vt(b"\x1b[48;2;255;0;0m\x1b#8");
    let frame = color.extract_frame().expect("test operation succeeds");
    assert_visible_text_rows(frame, &["EEE", "EEE", "EEE"])
        .expect("frame matches expected behavior");
    assert!(
        frame
            .cells
            .iter()
            .all(|cell| { cell.bg == Some(RgbColor { r: 255, g: 0, b: 0 }) })
    );
}

#[test]
fn terminal_engine_overwrites_the_line_on_carriage_return() {
    let mut engine = TerminalEngine::new(test_geometry(8, 2)).expect("test operation succeeds");

    engine.write_vt(b"AAAA\rBB\n");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        visible_text_rows(frame).expect("frame matches expected behavior"),
        vec!["BBAA".to_owned()]
    );
}

#[test]
fn terminal_engine_rewrites_a_status_line_without_duplicating_it() {
    let mut engine = TerminalEngine::new(test_geometry(40, 2)).expect("test operation succeeds");

    engine.write_vt(b"muse-spark-1.3-contributor\r");
    engine.write_vt(b"muse-spark-1.3-contributor\n");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        visible_text_rows(frame).expect("frame matches expected behavior"),
        vec!["muse-spark-1.3-contributor".to_owned()]
    );
}

#[test]
fn terminal_engine_scroll_region_shift_marks_every_moved_row_dirty() {
    let mut engine = TerminalEngine::new(test_geometry(4, 5)).expect("test operation succeeds");

    engine.write_vt(b"L1\r\nL2\r\nL3\r\nL4\r\nL5");
    engine.write_vt(b"\x1b[1;3r\x1b[3;1H\n");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        visible_text_rows(frame).expect("frame matches expected behavior"),
        vec![
            "L2".to_owned(),
            "L3".to_owned(),
            String::new(),
            "L4".to_owned(),
            "L5".to_owned(),
        ]
    );
    // The incremental paint cache repaints only rows flagged dirty; every row the
    // scroll moved must be flagged or stale text lingers beside fresh rows.
    assert!(frame.row_dirty[0] && frame.row_dirty[1] && frame.row_dirty[2]);
}

#[test]
fn terminal_engine_alternate_screen_redraw_keeps_plan_equal() {
    let mut engine = TerminalEngine::new(test_geometry(20, 6)).expect("test operation succeeds");

    engine.write_vt(b"primary line one\r\nprimary line two\r\n");
    // Enter the alternate screen, draw a fullscreen TUI frame, rewrite it.
    engine.write_vt(b"\x1b[?1049h");
    engine.write_vt(b"\x1b[H\x1b[2JTUI header");
    engine.write_vt(b"\x1b[6;1Hstatus one");
    let first = engine
        .extract_frame()
        .expect("test operation succeeds")
        .clone();
    engine.write_vt(b"\x1b[6;1H\x1b[2Kstatus two");
    let second = engine
        .extract_frame()
        .expect("test operation succeeds")
        .clone();

    assert_eq!(
        visible_text_rows(&second).expect("frame matches expected behavior"),
        vec![
            "TUI header".to_owned(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            "status two".to_owned(),
        ]
    );
    assert_ne!(first.row_dirty, second.row_dirty);
}

#[test]
fn terminal_engine_reverse_index_scrolls_the_region_down() {
    let mut engine = TerminalEngine::new(test_geometry(4, 5)).expect("test operation succeeds");

    engine.write_vt(b"L1\r\nL2\r\nL3\r\nL4\r\nL5");
    // Spinner-style rewrite: save cursor, scroll the top region down, restore.
    engine.write_vt(b"\x1b7\x1b[1;3r\x1b[1;1H\x1bM\x1b[r\x1b8");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        visible_text_rows(frame).expect("frame matches expected behavior"),
        vec![
            String::new(),
            "L1".to_owned(),
            "L2".to_owned(),
            "L4".to_owned(),
            "L5".to_owned(),
        ]
    );
}

#[test]
fn terminal_engine_supports_screen_active_view_scroll() {
    let mut engine = TerminalEngine::new(test_geometry(8, 2)).expect("test operation succeeds");

    engine.write_vt(b"one\r\ntwo\r\nthree");
    let frame = engine.extract_frame().expect("test operation succeeds");

    assert_eq!(
        occupied_cells(frame).expect("frame matches expected behavior"),
        [
            ('t', 0, 0),
            ('w', 1, 0),
            ('o', 2, 0),
            ('t', 0, 1),
            ('h', 1, 1),
            ('r', 2, 1),
            ('e', 3, 1),
            ('e', 4, 1),
        ]
    );
}
