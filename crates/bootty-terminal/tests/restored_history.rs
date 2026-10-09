#![cfg(unix)]

use std::{
    fmt::Write as _,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

use assert_fs::{TempDir, prelude::*};
use bootty_terminal::{
    SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
    geometry::TerminalGeometry,
    terminal_capture::{CaptureFormat, CaptureOptions, CaptureScope},
    terminal_engine::TerminalEngine,
    terminal_frame::CellStyle,
    terminal_history::{
        HistorySizeLimit, MAX_HISTORY_BYTES, capture_checkpoint, history_plain_text,
        sanitize_history, styled_history_bytes, validate_history,
    },
};
use pretty_assertions::assert_eq;
use rstest::rstest;

#[rstest]
fn restored_history_precedes_live_output_and_never_reaches_stdin() {
    let directory = TempDir::new().expect("private terminal fixture");
    let received = directory.child("received.txt");
    let (wake_tx, wake_rx) = mpsc::channel();
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".into()),
            args: vec!["-c".into(), "printf 'FRESH_OUTPUT\\n'; IFS= read -r line; printf '%s' \"$line\" > \"$1\"; printf 'ACCEPTED_INPUT:%s\\n' \"$line\"; IFS= read -r _".into(), "restored-test".into(), received.path().to_string_lossy().into_owned()],
            shell_integration: false,
            ..SessionLaunchConfig::default()
        },
        restored_history: Some(Arc::from("\x1b[1;38;2;10;20;30mSAVED_OUTPUT\nthis is presentation, never shell input\n")),
        max_scrollback: 100,
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(
        TerminalGeometry {
            cols: 80,
            rows: 24,
            cell_width: 8,
            cell_height: 16,
        },
        config,
        Arc::new(move || {
            let _ = wake_tx.send(());
        }),
    )
    .expect("restored terminal starts");
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    let mut capture;
    loop {
        capture = capture_history(&mut session).expect("restored terminal capture");
        if capture.text.contains("FRESH_OUTPUT") {
            break;
        }
        wake_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("terminal publishes before deadline");
    }
    let saved = capture
        .text
        .find("SAVED_OUTPUT")
        .expect("saved history retained");
    let live = capture
        .text
        .find("FRESH_OUTPUT")
        .expect("live process output retained");
    assert!(
        saved < live,
        "history must precede the new process's output"
    );
    assert!(!received.path().exists());
    // History queries read the worker; displayed frames publish independently.
    let frame = loop {
        let frame = session.extract_frame().expect("styled restored frame");
        if frame
            .text_rows()
            .iter()
            .any(|row| row.contains("FRESH_OUTPUT"))
        {
            break frame;
        }
        wake_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("restored frame publishes before deadline");
    };
    let saved_cell = frame
        .cells
        .iter()
        .find(|cell| frame.cell_text(cell) == ['S'])
        .expect("saved styled cell");
    assert_eq!(
        saved_cell.fg,
        Some(bootty_terminal::RgbColor {
            r: 10,
            g: 20,
            b: 30
        })
    );
    assert!(saved_cell.style.bold);
    let fresh_cell = frame
        .cells
        .iter()
        .find(|cell| frame.cell_text(cell) == ['F'])
        .expect("fresh output cell");
    assert_eq!(fresh_cell.style, CellStyle::default());
    assert_eq!(fresh_cell.fg, None);
    session
        .write_input(b"EXPLICIT_INPUT\n")
        .expect("explicit input queues");
    loop {
        let capture = capture_history(&mut session).expect("accepted input capture");
        if capture.text.contains("ACCEPTED_INPUT:EXPLICIT_INPUT") {
            break;
        }
        wake_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("terminal publishes before deadline");
    }
    assert_eq!(
        std::fs::read_to_string(received.path()).expect("child received explicit input"),
        "EXPLICIT_INPUT"
    );
}

#[rstest]
#[case::osc("\x1b]52;c;YWJj\x07")]
#[case::query("\x1b[6n")]
#[case::cursor("\x1b[2J")]
#[case::mode("\x1b[?1049h")]
#[case::dcs("\x1bP$qm\x1b\\")]
#[case::malformed_style("\x1b[38;2;256;0;0m")]
#[case::incomplete_color("\x1b[48;5m")]
#[case::private_style("\x1b[>4;2m")]
#[case::too_large(&"x".repeat(256*1024+1))]
fn invalid_history_is_rejected_before_the_process_runs(#[case] history: &str) {
    let directory = TempDir::new().expect("private terminal fixture");
    let launched = directory.child("launched");
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".into()),
            args: vec![
                "-c".into(),
                "touch \"$1\"".into(),
                "invalid-history-test".into(),
                launched.path().to_string_lossy().into_owned(),
            ],
            ..SessionLaunchConfig::default()
        },
        restored_history: Some(Arc::from(history)),
        ..TerminalSessionConfig::default()
    };
    assert!(
        TerminalSession::new_with_config(
            TerminalGeometry {
                cols: 80,
                rows: 24,
                cell_width: 8,
                cell_height: 16
            },
            config,
            Arc::new(|| {})
        )
        .is_err()
    );
    assert!(!launched.path().exists());
}

fn capture_history(
    session: &mut TerminalSession,
) -> anyhow::Result<bootty_terminal::terminal_capture::TerminalCapture> {
    session
        .capture(CaptureOptions {
            scope: CaptureScope::History,
            ..CaptureOptions::default()
        })?
        .receive("history capture")?
        .map_err(anyhow::Error::msg)
}

#[rstest]
fn formatted_history_round_trips_rendered_colors_and_styles() {
    let geometry = TerminalGeometry {
        cols: 80,
        rows: 8,
        cell_width: 8,
        cell_height: 16,
    };
    let mut original = TerminalEngine::new(geometry).expect("source engine");
    original.write_vt(b"\x1b]4;123;rgb:ab/cd/ef\x1b\\\x1b[1;3;4:3;38;2;10;20;30;48;5;45mRGB\x1b[0m plain\r\n\x1b[2;7;9;53;38;5;123mINDEXED\x1b[0m");
    let capture = original
        .capture(CaptureOptions {
            scope: CaptureScope::History,
            format: CaptureFormat::Ansi,
            ..Default::default()
        })
        .expect("formatted saved history");
    let saved = sanitize_history(&capture.text).expect("safe styled checkpoint");
    validate_history(&saved).expect("persisted style validation");
    let mut restored = TerminalEngine::new(geometry).expect("restored engine");
    restored.write_vt_without_pty_responses(&styled_history_bytes(&saved).expect("restore bytes"));
    let visible = |engine: &mut TerminalEngine| {
        let frame = engine.extract_frame().expect("render frame");
        frame
            .cells
            .iter()
            .filter(|cell| {
                let text = frame.cell_text(cell);
                !text.is_empty() && text.iter().any(|ch| !ch.is_whitespace())
            })
            .map(|cell| (frame.cell_text(cell).to_vec(), cell.fg, cell.bg, cell.style))
            .collect::<Vec<_>>()
    };
    assert_eq!(visible(&mut restored), visible(&mut original));
    assert_eq!(
        history_plain_text(&saved).unwrap().trim_end(),
        "RGB plain\r\nINDEXED"
    );
}

#[rstest]
#[case::clipboard("\x1b]52;c;YWJj\x07")]
#[case::hyperlink("\x1b]8;;https://example.com\x1b\\")]
#[case::palette("\x1b]4;1;rgb:ff/00/00\x1b\\")]
fn fresh_capture_discards_osc_without_losing_text_or_styles(#[case] osc: &str) {
    let capture = format!("before{osc}\x1b[31mcolored\x1b[0mafter");
    assert!(validate_history(&capture).is_err());
    assert_eq!(
        sanitize_history(&capture).unwrap(),
        "before\x1b[31mcolored\x1b[0mafter"
    );
}

#[rstest]
#[case("\x1b]8;;unterminated")]
#[case("\x1b]8;;bad\x1b[31m")]
#[case("\x1b[31")]
#[case("\x1b[4:6m")]
#[case("\x1b[38;3;1m")]
#[case("\x1b[38;2;0;;0m")]
#[case("\x1b[10m")]
#[case("\u{009b}31m")]
fn malformed_or_non_presentation_capture_is_rejected(#[case] text: &str) {
    assert!(sanitize_history(text).is_err());
}

#[rstest]
fn plain_history_contract_still_rejects_styles() {
    assert!(bootty_terminal::terminal_session::plain_history_bytes("\x1b[31mcolored").is_err());
    assert_eq!(
        styled_history_bytes("old plain\ncheckpoint").unwrap(),
        bootty_terminal::terminal_session::plain_history_bytes("old plain\ncheckpoint").unwrap()
    );
}

#[rstest]
fn checkpoint_captures_a_fresh_whole_styled_tail_instead_of_retaining_an_oversized_snapshot() {
    let mut engine = TerminalEngine::new_with_scrollback(
        TerminalGeometry {
            cols: 80,
            rows: 4,
            cell_width: 8,
            cell_height: 16,
        },
        bootty_terminal::terminal_engine::TerminalColorConfig::default(),
        16 * 1024 * 1024,
    )
    .unwrap();
    let mut output = String::new();
    let padding = "x".repeat(60);
    for row in 0..5_000 {
        writeln!(output, "\x1b[38;5;123mrow-{row:04}: {padding}\x1b[0m\r")
            .expect("styled fixture output");
    }
    engine.write_vt(output.as_bytes());
    let options = CaptureOptions {
        scope: CaptureScope::History,
        format: CaptureFormat::Ansi,
        max_lines: 10_000,
        max_bytes: MAX_HISTORY_BYTES,
        ..Default::default()
    };
    let oversized = engine
        .capture(options)
        .expect_err("full styled history exceeds budget");
    assert!(oversized.is::<HistorySizeLimit>());
    let all = engine
        .capture(CaptureOptions {
            format: CaptureFormat::Plain,
            max_bytes: 1024 * 1024,
            ..options
        })
        .unwrap();
    let checkpoint = engine
        .capture_checkpoint(options)
        .expect("latest bounded complete rows");
    assert!(checkpoint.text.len() <= MAX_HISTORY_BYTES);
    validate_history(&checkpoint.text).unwrap();
    assert!(checkpoint.text.contains("row-4999"));
    assert!(!checkpoint.text.contains("row-0000"));
    assert!(checkpoint.omitted_lines > 0);
    assert_eq!(
        checkpoint
            .omitted_lines
            .checked_add(u64::from(checkpoint.captured_lines))
            .expect("total history lines fit u64"),
        u64::from(all.captured_lines)
    );
    assert!(
        checkpoint.text.contains("\x1b[38;2;"),
        "palette colors stay explicit after bounding"
    );
}

#[rstest]
fn checkpoint_backend_capture_retries_rgb_expansion_and_keeps_accurate_omissions() {
    let options = CaptureOptions {
        scope: CaptureScope::History,
        format: CaptureFormat::Ansi,
        max_lines: 4,
        max_bytes: 60,
        ..Default::default()
    };
    let mut attempts = Vec::new();
    let checkpoint = capture_checkpoint(options, |options| {
        attempts.push(options.max_lines);
        let retained = options.max_lines.min(4);
        Ok(bootty_terminal::terminal_capture::TerminalCapture {
            cols: 0,
            rows: 0,
            scope: options.scope,
            format: options.format,
            alternate_screen: false,
            captured_lines: retained,
            omitted_lines: u64::from(
                4_u32
                    .checked_sub(retained)
                    .expect("retained rows fit fixture"),
            ),
            text: format!(
                "\x1b]4;1;rgb:ff/aa/bb\x1b\\{}",
                "\x1b[38;5;1mX\r\n".repeat(usize::try_from(retained).unwrap())
            ),
        })
    })
    .unwrap();
    assert!(attempts.len() > 1);
    assert_eq!(
        checkpoint
            .captured_lines
            .checked_add(u32::try_from(checkpoint.omitted_lines).unwrap())
            .expect("total history lines fit u32"),
        4
    );
    assert!(checkpoint.text.len() <= options.max_bytes);
    assert!(checkpoint.text.contains("\x1b[38;2;255;170;187mX"));
}

#[rstest]
#[case::oversized("x".repeat(MAX_HISTORY_BYTES+1), 100_000, 17)]
#[case::single_wide_row("x".repeat(MAX_HISTORY_BYTES+1), 1, 1)]
#[case::query("\x1b[6n".into(), 100_000, 1)]
fn checkpoint_retries_are_finite_and_never_retry_unsafe_controls(
    #[case] text: String,
    #[case] max_lines: u32,
    #[case] max_attempts: usize,
) {
    let options = CaptureOptions {
        scope: CaptureScope::History,
        format: CaptureFormat::Ansi,
        max_lines,
        max_bytes: MAX_HISTORY_BYTES,
        ..Default::default()
    };
    let mut attempts = Vec::new();
    let result = capture_checkpoint(options, |options| {
        attempts.push(options.max_lines);
        Ok(bootty_terminal::terminal_capture::TerminalCapture {
            cols: 0,
            rows: 0,
            scope: options.scope,
            format: options.format,
            alternate_screen: false,
            captured_lines: 1,
            omitted_lines: 0,
            text: text.clone(),
        })
    });
    assert!(result.is_err());
    assert!(attempts.len() <= max_attempts);
    if max_attempts > 1 {
        assert_eq!(attempts.last(), Some(&1));
    } else {
        assert_eq!(attempts.len(), 1);
    }
}

proptest::proptest! {
    #[test]
    fn restored_rgb_channels_preserve_the_style_and_displayed_text(r: u8, g: u8, b: u8, text in "[a-zA-Z0-9]{1,32}") {
        let saved = format!("\x1b[38;2;{r};{g};{b}m{text}");
        proptest::prop_assert!(validate_history(&saved).is_ok());
        let plain = history_plain_text(&saved).unwrap();
        proptest::prop_assert_eq!(plain.as_str(), text.as_str());
        let mut engine = TerminalEngine::new(TerminalGeometry { cols: 80, rows: 3, cell_width: 8, cell_height: 16 }).unwrap();
        engine.write_vt_without_pty_responses(&styled_history_bytes(&saved).unwrap());
        let frame = engine.extract_frame().unwrap();
        let visible = frame.cells.iter().filter(|cell| !frame.cell_text(cell).is_empty() && frame.cell_text(cell).iter().any(|ch| !ch.is_whitespace())).collect::<Vec<_>>();
        proptest::prop_assert_eq!(visible.len(), text.len());
        proptest::prop_assert!(visible.iter().all(|cell| cell.fg == Some(bootty_terminal::RgbColor { r, g, b })), "restored foreground differs from saved RGB");
    }
}
