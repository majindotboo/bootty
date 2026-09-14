use bootty_terminal::{
    geometry::TerminalGeometry,
    terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope},
    terminal_engine::{TerminalColorConfig, TerminalEngine},
};
use pretty_assertions::assert_eq;
use rstest::rstest;

fn engine() -> anyhow::Result<TerminalEngine> {
    TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 20,
            rows: 3,
            cell_width: 8,
            cell_height: 16,
        },
        TerminalColorConfig::default(),
        65536,
    )
}

#[rstest]
#[case(CaptureFormat::Plain)]
#[case(CaptureFormat::Ansi)]
#[case(CaptureFormat::Html)]
fn capture_includes_retained_history_without_changing_selection(#[case] format: CaptureFormat) {
    let mut engine = engine().expect("terminal fixture");
    engine.write_vt(b"\x1b[31mold-token\x1b[0m\r\nsecond\r\nthird\r\nfourth\r\nlatest-token");
    let options = CaptureOptions {
        format,
        scope: CaptureScope::History,
        ..Default::default()
    };
    let history = engine.capture(options).unwrap();
    assert!(history.text.contains("old-token"));
    assert!(history.text.contains("latest-token"));
    let screen = engine
        .capture(CaptureOptions {
            scope: CaptureScope::Screen,
            ..options
        })
        .unwrap();
    assert!(!screen.text.contains("old-token"));
    assert!(screen.text.contains("latest-token"));
    assert_eq!(
        engine
            .format_selection(bootty_terminal::terminal_engine::TerminalSelectionFormat::PlainText)
            .unwrap(),
        None
    );
    match format {
        CaptureFormat::Plain => assert!(!history.text.contains('\x1b')),
        CaptureFormat::Ansi => assert!(history.text.contains('\x1b')),
        CaptureFormat::Html => assert!(history.text.contains('<')),
    }
}

#[rstest]
fn limits_select_latest_rows_and_reject_oversized_encoded_output() {
    let mut engine = engine().expect("terminal fixture");
    engine.write_vt("old\r\nprevious\r\nlast: 🦀".as_bytes());
    let options = CaptureOptions {
        scope: CaptureScope::History,
        max_lines: 1,
        ..Default::default()
    };
    let capture = engine.capture(options).unwrap();
    assert_eq!(capture.text, "last: 🦀");
    assert_eq!(capture.captured_lines, 1);
    assert_eq!(capture.omitted_lines, 2);
    assert!(
        engine
            .capture(CaptureOptions {
                max_bytes: 2,
                ..options
            })
            .unwrap_err()
            .to_string()
            .contains("exceeding")
    );
    assert!(
        engine
            .capture(CaptureOptions {
                max_lines: 0,
                ..options
            })
            .is_err()
    );
}

#[rstest]
fn alternate_screen_capture_does_not_claim_primary_history() {
    let mut engine = engine().expect("terminal fixture");
    engine.write_vt(b"primary\x1b[?1049halt");
    let capture = engine
        .capture(CaptureOptions {
            scope: CaptureScope::History,
            ..Default::default()
        })
        .unwrap();
    assert!(capture.alternate_screen);
    assert!(capture.text.contains("alt"));
    assert!(!capture.text.contains("primary"));
}

#[rstest]
fn capture_preserves_active_selection_and_escapes_html_text() {
    use bootty_terminal::{
        geometry::{CellMetrics, SurfacePoint, TerminalPadding, TerminalSurface},
        terminal_engine::{TerminalSelectionEvent, TerminalSelectionFormat},
    };
    let mut engine = engine().expect("terminal fixture");
    engine.write_vt(b"<script> & text");
    let surface = TerminalSurface::for_logical_size(
        160.0,
        48.0,
        CellMetrics::new(8.0, 16.0),
        TerminalPadding::default(),
    );
    let event = |x| TerminalSelectionEvent {
        surface,
        position: SurfacePoint { x, y: 8.0 },
        rectangle: false,
    };
    engine.begin_selection(event(0.0)).unwrap();
    engine.update_selection(event(56.0)).unwrap();
    engine.end_selection(None).unwrap();
    let before = engine
        .format_selection(TerminalSelectionFormat::PlainText)
        .unwrap();
    assert!(before.is_some());
    let capture = engine
        .capture(CaptureOptions {
            format: CaptureFormat::Html,
            ..Default::default()
        })
        .unwrap();
    assert!(!capture.text.contains("<script>"));
    assert!(capture.text.contains("&lt;script&gt;"));
    assert_eq!(
        engine
            .format_selection(TerminalSelectionFormat::PlainText)
            .unwrap(),
        before
    );
}

proptest::proptest! {
    #[test]
    fn unwrapped_capture_round_trips_soft_wrapped_text(text in "[a-zA-Z0-9]{1,256}") {
        let mut engine = engine().expect("terminal fixture");
        engine.write_vt(text.as_bytes());
        let captured = engine.capture(CaptureOptions { scope: CaptureScope::History, ..Default::default() }).unwrap();
        proptest::prop_assert_eq!(captured.text.trim_end(), text);
    }
}
