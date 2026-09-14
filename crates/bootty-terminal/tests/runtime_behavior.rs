#[cfg(unix)]
use std::time::Instant;
use std::{fs, time::Duration};
#[cfg(unix)]
use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
};

use assert_fs::{TempDir, prelude::*};
#[cfg(unix)]
use bootty_terminal::frame_source::TerminalFrameSource;
#[cfg(unix)]
use bootty_terminal::geometry::CellMetrics;
use bootty_terminal::terminal_engine::TerminalEngine;
#[cfg(unix)]
use bootty_terminal::terminal_engine::{
    TERMINAL_PROGRAM_VERSION, TerminalCopyModeAction, TerminalSearchDirection,
    TerminalSelectionFormat,
};
#[cfg(unix)]
use bootty_terminal::terminal_input_model::{KeyInput, KeyMods, TerminalKey};
use bootty_terminal::{
    BenchmarkTrace, OutputBacklog, SessionLaunchConfig, TerminalSession, TerminalSessionConfig,
    TraceValue, drain_output_backlog, drain_output_backlog_with_limits,
    geometry::TerminalGeometry,
    perf::{guard_frame_path, record_subprocess},
    scheduler::{RepaintScheduler, RepaintSignal},
    terminal_frame::CursorSnapshot,
    terminal_session::{
        CURSOR_COMMIT_DELAY, CursorHold, PublishHold, WORKER_OUTPUT_HOLD_MAX, WORKER_OUTPUT_QUIET,
        output_settling, should_publish_frame_after_work,
    },
};
use pretty_assertions::{assert_eq, assert_ne};
use proptest::prelude::*;
use proptest_derive::Arbitrary;

#[derive(Arbitrary, Debug)]
struct BacklogCase {
    #[proptest(
        strategy = "prop::collection::vec(prop::collection::vec(any::<u8>(), 1..256), 0..16)"
    )]
    chunks: Vec<Vec<u8>>,
    #[proptest(strategy = "1usize..16_384")]
    byte_limit: usize,
    #[proptest(strategy = "1usize..8")]
    chunk_limit: usize,
    interleave_reads: bool,
}

const fn geometry(cols: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry {
        cols,
        rows,
        cell_width: 8,
        cell_height: 16,
    }
}

#[test]
fn scheduler_prioritizes_input_over_idle_chrome() {
    let scheduler = RepaintScheduler::default();
    let idle = scheduler.recommend(RepaintSignal {
        drained_bytes: 0,
        drain_elapsed_us: 0,
        pending_bytes: 0,
        dirty_rows: 0,
        cursor_blinking: false,
        input_commands: 0,
    });
    let input = scheduler.recommend(RepaintSignal {
        input_commands: 1,
        ..RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        }
    });
    let busy = scheduler.recommend(RepaintSignal {
        pending_bytes: 1,
        ..RepaintSignal {
            drained_bytes: 0,
            drain_elapsed_us: 0,
            pending_bytes: 0,
            dirty_rows: 0,
            cursor_blinking: false,
            input_commands: 0,
        }
    });

    assert_eq!(idle, Duration::from_millis(900));
    assert_eq!(input, Duration::ZERO);
    assert_eq!(busy, Duration::from_millis(16));
}

#[test]
#[should_panic(expected = "git status spawned a subprocess on the frame path")]
fn frame_path_guard_names_a_forbidden_subprocess() {
    let _guard = guard_frame_path();
    record_subprocess("git status");
}

#[test]
fn benchmark_trace_writes_sampled_json_lines() {
    let directory = TempDir::new().expect("trace directory");
    let path = directory.child("trace.jsonl");
    let trace = BenchmarkTrace::create(path.path(), 2).expect("trace opens");

    trace.emit("first", &[("count", TraceValue::Usize(1))]);
    trace.emit("skipped", &[("count", TraceValue::Usize(2))]);
    trace.emit("third", &[("ready", TraceValue::Bool(true))]);
    drop(trace);

    let lines = fs::read_to_string(path.path()).expect("trace reads");
    let normalized = lines
        .lines()
        .map(|line| {
            let (header, event) = line.split_once(",\"event\":").expect("trace event field");
            let timestamp = header
                .strip_prefix("{\"schema_version\":1,\"ts_ns\":")
                .expect("trace schema header");
            assert!(timestamp.parse::<u128>().is_ok(), "timestamp: {timestamp}");
            format!("{{\"event\":{event}")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        normalized,
        [
            r#"{"event":"first","count":1}"#,
            r#"{"event":"third","ready":true}"#,
        ]
    );
}

proptest! {
    /// Property: bounded drains make progress, respect both limits, preserve byte order,
    /// and eventually leave no pending bytes even when a source chunk must be split.
    #[test]
    fn bounded_backlog_drain_preserves_all_bytes(case in any::<BacklogCase>()) {
        let expected = case.chunks.iter().flatten().copied().collect::<Vec<_>>();
        let mut backlog = OutputBacklog::new();
        let mut chunks = case.chunks.into_iter();
        if !case.interleave_reads {
            for chunk in chunks.by_ref() {
                backlog.push_back(chunk);
            }
        }
        let mut observed = Vec::new();

        while !backlog.is_empty() || chunks.len() > 0 {
            // Appending after a partial drain must preserve the unread tail.
            if let Some(chunk) = chunks.next() {
                backlog.push_back(chunk);
            }
            let before = backlog.len();
            let stats = drain_output_backlog_with_limits(
                &mut backlog,
                case.byte_limit,
                case.chunk_limit,
                u128::MAX,
                |bytes| observed.extend_from_slice(bytes),
            );
            prop_assert!(stats.bytes > 0);
            prop_assert!(stats.bytes <= case.byte_limit);
            prop_assert!(stats.chunks <= case.chunk_limit);
            prop_assert_eq!(backlog.len(), before.checked_sub(stats.bytes).expect("drained bytes are queued"));
        }

        prop_assert_eq!(&observed, &expected);
    }
}

#[test]
fn bounded_pty_drain_preserves_split_synchronized_output_control() {
    let geometry = geometry(80, 24);
    let mut engine = TerminalEngine::new(geometry).expect("terminal engine");
    let mut backlog = OutputBacklog::new();
    let mut bytes = vec![b'x'; 65535];
    bytes.extend_from_slice(b"\x1b[?2026h");
    backlog.push_back(bytes);
    let mut slices = Vec::new();

    let stats = drain_output_backlog(&mut backlog, |slice| {
        slices.push(slice.to_vec());
        engine.write_vt(slice);
    });

    assert_eq!(stats.bytes, 65543);
    assert_eq!(slices.len(), 2);
    assert_eq!(slices[0].len(), 65536);
    assert_eq!(slices[0].last(), Some(&0x1b));
    assert_eq!(slices[1], b"[?2026h");
    assert!(
        engine
            .is_synchronized_output()
            .expect("query synchronized output mode")
    );
}

#[test]
fn completed_synchronized_output_batch_suppresses_intermediate_publish() {
    let geometry = geometry(80, 24);
    let mut engine = TerminalEngine::new(geometry).expect("terminal engine");
    let mut backlog = OutputBacklog::new();
    backlog.push_back(b"\x1b[?2026hredraw\x1b[?2026l".to_vec());
    let mut observed = false;

    let stats = drain_output_backlog(&mut backlog, |slice| {
        engine.write_vt(slice);
        observed |= engine.take_synchronized_output_observed();
    });

    assert_eq!(stats.bytes, 22);
    assert!(observed);
    assert!(
        !engine
            .is_synchronized_output()
            .expect("query synchronized output mode")
    );
    assert!(
        bootty_terminal::terminal_session::sync_output_suppresses_publish(
            false,
            observed,
            Duration::ZERO,
        )
    );
}

#[test]
fn continuous_drained_output_publishes_at_the_ready_interval() {
    assert!(should_publish_frame_after_work(
        true,
        false,
        PublishHold::None,
        0,
        Duration::ZERO,
        Duration::from_millis(16),
    ));
}

#[rstest::rstest]
#[case::output_still_arriving(Duration::ZERO, Duration::ZERO, true)]
#[case::output_went_quiet(WORKER_OUTPUT_QUIET, Duration::ZERO, false)]
#[case::hold_cap_reached(Duration::ZERO, WORKER_OUTPUT_HOLD_MAX, false)]
fn quiet_window_waits_for_quiet_but_never_past_the_cap(
    #[case] since_last_change: Duration,
    #[case] held_for: Duration,
    #[case] expected: bool,
) {
    assert_eq!(output_settling(since_last_change, held_for), expected);
}

#[allow(
    clippy::unnecessary_wraps,
    reason = "mirrors the Option<CursorSnapshot> frame field"
)]
const fn cursor_at(x: u16, y: u16) -> Option<CursorSnapshot> {
    Some(CursorSnapshot {
        x,
        y,
        at_wide_tail: false,
        style: libghostty_vt::render::CursorVisualStyle::Block,
        blinking: false,
        color: None,
    })
}

#[test]
fn cursor_hold_never_shows_a_cross_row_move_superseded_within_the_commit_delay() {
    let mut hold = CursorHold::default();
    let t0 = Instant::now();
    // First frame initializes at the input row.
    assert_eq!(hold.resolve(cursor_at(2, 23), t0), cursor_at(2, 23));
    // A jump to a status row is held: the caret stays at the input row.
    assert_eq!(
        hold.resolve(cursor_at(99, 1), t0 + Duration::from_millis(1)),
        cursor_at(2, 23)
    );
    assert!(hold.pending());
    // The app moves the cursor back to the input row before the delay: the park is never shown.
    assert_eq!(
        hold.resolve(cursor_at(2, 23), t0 + Duration::from_millis(3)),
        cursor_at(2, 23)
    );
    assert!(!hold.pending());
}

#[test]
fn cursor_hold_commits_a_cross_row_move_that_survives_the_commit_delay() {
    let mut hold = CursorHold::default();
    let t0 = Instant::now();
    hold.resolve(cursor_at(2, 23), t0);
    assert_eq!(
        hold.resolve(cursor_at(40, 10), t0 + Duration::from_millis(1)),
        cursor_at(2, 23),
        "a fresh cross-row move is held"
    );
    assert!(hold.commit_due(t0 + CURSOR_COMMIT_DELAY + Duration::from_millis(1)));
    assert_eq!(
        hold.resolve(
            cursor_at(40, 10),
            t0 + CURSOR_COMMIT_DELAY + Duration::from_millis(1)
        ),
        cursor_at(40, 10),
        "a cross-row move that outlasts the delay becomes visible"
    );
}

#[test]
fn cursor_hold_shows_same_row_caret_advances_immediately() {
    let mut hold = CursorHold::default();
    let t0 = Instant::now();
    assert_eq!(hold.resolve(cursor_at(2, 23), t0), cursor_at(2, 23));
    // Typing advances the caret along the input row: every step is visible at once, no lag.
    assert_eq!(
        hold.resolve(cursor_at(3, 23), t0 + Duration::from_millis(1)),
        cursor_at(3, 23)
    );
    assert_eq!(
        hold.resolve(cursor_at(4, 23), t0 + Duration::from_millis(2)),
        cursor_at(4, 23)
    );
    assert!(!hold.pending());
}

// A keystroke can arrive while a status park is the live cursor. Judging by row means the park,
// on a different row, stays held rather than being committed by the keystroke's publish.
#[test]
fn cursor_hold_does_not_let_a_live_park_commit_while_held() {
    let mut hold = CursorHold::default();
    let t0 = Instant::now();
    hold.resolve(cursor_at(2, 23), t0);
    assert_eq!(
        hold.resolve(cursor_at(99, 1), t0 + Duration::from_millis(1)),
        cursor_at(2, 23)
    );
    // A publish a couple of milliseconds later, still within the delay, keeps holding.
    assert_eq!(
        hold.resolve(cursor_at(99, 1), t0 + Duration::from_millis(3)),
        cursor_at(2, 23)
    );
    assert!(hold.pending());
}

#[test]
fn cursor_hold_commits_a_visibility_change_immediately() {
    let mut hold = CursorHold::default();
    let t0 = Instant::now();
    hold.resolve(cursor_at(2, 23), t0);
    assert_eq!(hold.resolve(None, t0 + Duration::from_millis(1)), None);
}

proptest! {
    #[test]
    fn cursor_parks_obey_the_commit_boundary(elapsed_us in 0_u64..20_000) {
        let mut hold = CursorHold::default();
        let started = Instant::now();
        let home = cursor_at(2, 23);
        let parked = cursor_at(98, 0);
        prop_assert_eq!(hold.resolve(home, started), home);
        prop_assert_eq!(hold.resolve(parked, started), home);
        let elapsed = Duration::from_micros(elapsed_us);
        let now = started.checked_add(elapsed).expect("test timestamp");
        let due = elapsed >= CURSOR_COMMIT_DELAY;
        prop_assert_eq!(hold.commit_due(now), due);
        prop_assert_eq!(hold.resolve(parked, now), if due { parked } else { home });
        if !due {
            prop_assert_eq!(hold.resolve(home, now), home);
            prop_assert!(!hold.pending());
        }
    }
}

#[rstest::rstest]
#[case::quiet_window_holds_input_echo(true, PublishHold::Settling, 0, false)]
#[case::sync_output_holds_input_echo(true, PublishHold::SyncOutput, 0, false)]
#[case::input_echo_skips_frame_pacing(true, PublishHold::None, 0, true)]
fn publish_holds_rank_above_input_echo(
    #[case] force: bool,
    #[case] hold: PublishHold,
    #[case] pending: usize,
    #[case] expected: bool,
) {
    assert_eq!(
        should_publish_frame_after_work(
            true,
            force,
            hold,
            pending,
            Duration::ZERO,
            Duration::from_millis(16),
        ),
        expected
    );
}

// One logical redraw split across write(2) calls from the same process, a fraction of a
// millisecond apart, as tmux and pi-tui do. pi-tui closes its synchronized-output block and then
// positions the cursor; tmux positions the cursor and then opens its block. A frame published inside
// either gap parks the cursor mid-screen. The writer is this test binary (see
// `redraw_writer_helper`), because a shell cannot space two writes that closely.
const REDRAW_WRITER_ROWS: &str = "BOOTTY_TEST_REDRAW_ROWS";
const REDRAW_WRITER_LEAD: &str = "BOOTTY_TEST_REDRAW_LEAD";
const REDRAW_WRITER_INNER_END: &str = "BOOTTY_TEST_REDRAW_INNER_END";
const REDRAW_WRITER_TRAIL: &str = "BOOTTY_TEST_REDRAW_TRAIL";
const REDRAW_WRITER_DONE: &str = "BOOTTY_TEST_REDRAW_DONE";
const REDRAW_WRITE_GAP: Duration = Duration::from_micros(150);

#[test]
fn redraw_writer_helper() {
    use std::io::Write as _;
    let Ok(rows) = std::env::var(REDRAW_WRITER_ROWS) else {
        return;
    };
    let rows: usize = rows.parse().expect("row count");
    let lead = std::env::var(REDRAW_WRITER_LEAD).unwrap_or_default();
    let inner_end = std::env::var(REDRAW_WRITER_INNER_END).unwrap_or_default();
    let trail = std::env::var(REDRAW_WRITER_TRAIL).unwrap_or_default();
    let row = format!("\x1b[2K{}\x1b[B\r", "x".repeat(150));
    let block = format!(
        "\x1b[?2026h\x1b[H{}{inner_end}\x1b[?2026l",
        row.repeat(rows)
    );
    let mut out = std::io::stdout().lock();
    let mut write = |bytes: &str| {
        out.write_all(bytes.as_bytes()).expect("pty write");
        out.flush().expect("pty flush");
    };
    for _ in 0..13 {
        if !lead.is_empty() {
            write(&lead);
            thread::sleep(REDRAW_WRITE_GAP);
        }
        write(&block);
        if !trail.is_empty() {
            thread::sleep(REDRAW_WRITE_GAP);
            write(&trail);
        }
        // Past the worker's ready interval, so each redraw's first write is publishable on its own.
        thread::sleep(Duration::from_millis(20));
    }
    // Keep the PTY alive until the parent has stopped queuing keys and inspected its frames.
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let path = std::env::var_os(REDRAW_WRITER_DONE).expect("redraw completion socket");
        let mut completion =
            std::os::unix::net::UnixStream::connect(path).expect("redraw completion");
        let _ = completion.read(&mut [0]);
    }
}

#[cfg(unix)]
#[rstest::rstest]
#[case::pi_trailing_cursor_move_small(1, "", "\x1b[A", "\x1b[5B\x1b[3G", (2, 5), false)]
#[case::pi_trailing_cursor_move_full(20, "", "\x1b[A", "\x1b[5B\x1b[3G", (2, 24), false)]
#[case::tmux_leading_cursor_move(1, "\x1b[28;197H", "\x1b[29;3H", "", (2, 28), false)]
#[case::tmux_leading_cursor_move_while_typing(1, "\x1b[28;197H", "\x1b[29;3H", "", (2, 28), true)]
fn frames_never_show_the_cursor_between_writes_of_one_redraw(
    #[case] rows: usize,
    #[case] lead: &str,
    #[case] inner_end: &str,
    #[case] trail: &str,
    #[case] cursor_after_redraw: (u16, u16),
    #[case] typing: bool,
) {
    let control_directory = TempDir::new().expect("redraw control directory");
    let completion_path = control_directory.path().join("done");
    let completion = std::os::unix::net::UnixListener::bind(&completion_path)
        .expect("redraw completion listener");
    completion
        .set_nonblocking(true)
        .expect("nonblocking completion");
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some(
                std::env::current_exe()
                    .expect("test binary path")
                    .to_string_lossy()
                    .into_owned(),
            ),
            args: vec![
                "--exact".into(),
                "redraw_writer_helper".into(),
                "--nocapture".into(),
            ],
            env: vec![
                (
                    REDRAW_WRITER_DONE.into(),
                    completion_path.to_string_lossy().into_owned(),
                ),
                (REDRAW_WRITER_ROWS.into(), rows.to_string()),
                (REDRAW_WRITER_LEAD.into(), lead.into()),
                (REDRAW_WRITER_INNER_END.into(), inner_end.into()),
                (REDRAW_WRITER_TRAIL.into(), trail.into()),
            ],
            shell_integration: false,
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let (repaint_tx, repaint_rx) = std::sync::mpsc::channel();
    let mut session = TerminalSession::new_with_config(
        geometry(200, 30),
        config,
        Arc::new(move || {
            let _ = repaint_tx.send(());
        }),
    )
    .expect("terminal starts");

    let mut cursors = Vec::new();
    let mut completed = None;
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    while Instant::now() < deadline {
        match completion.accept() {
            Ok((stream, _)) => {
                completed = Some(stream);
                break;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
            Err(error) => panic!("redraw completion failed: {error}"),
        }
        let exited = session.child_exited().expect("child status reads");
        if typing && !exited {
            // Keystrokes arm the input fast path, which must not publish the redraw's first write.
            session
                .encode_key(KeyInput {
                    key: TerminalKey::A,
                    mods: KeyMods::default(),
                    repeat: false,
                    utf8: Some("a"),
                    unshifted: Some('a'),
                })
                .expect("key queues");
        }
        if repaint_rx.recv_timeout(Duration::from_millis(5)).is_err() {
            if exited {
                break;
            }
            continue;
        }
        let frame = session.extract_frame().expect("frame extracts");
        if let Some(cursor) = &frame.cursor {
            cursors.push((cursor.x, cursor.y));
        }
    }
    assert!(completed.is_some(), "redraw writer did not complete");
    drop(session);
    drop(completed);
    // The pty echoes typed keys, which walks the cursor along its row between redraws.
    let at_rest = |cursor: (u16, u16)| {
        if typing {
            cursor.1 == cursor_after_redraw.1
        } else {
            cursor == cursor_after_redraw
        }
    };
    // libtest's "running 1 test" header lands before the first redraw; judge from the first
    // completed redraw onward.
    let first_redraw = cursors
        .iter()
        .position(|&cursor| at_rest(cursor))
        .unwrap_or(cursors.len());
    let after_first = &cursors[first_redraw..];
    assert!(
        after_first.len() >= 6,
        "expected one frame per redraw, saw {cursors:?}"
    );
    assert!(
        after_first.iter().all(|&cursor| at_rest(cursor)),
        "a frame was published between two writes of one redraw: {cursors:?}"
    );
}

#[cfg(unix)]
#[test]
fn carriage_return_rewrite_publishes_a_single_row() {
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "printf 'STATUS-LINE\\rSTATUS-LINE\\n'".to_owned(),
            ],
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(geometry(40, 4), config, Arc::new(|| {}))
        .expect("terminal starts");

    for _ in 0..100 {
        if session.child_exited().expect("child status reads") {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    // Let the worker drain the exited child's final bytes into a published frame.
    let deadline = Instant::now() + Duration::from_secs(2);
    let rows = loop {
        let rows = session
            .extract_frame()
            .expect("frame reads")
            .text_rows()
            .into_iter()
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>();
        if rows == ["STATUS-LINE".to_owned()] || Instant::now() > deadline {
            break rows;
        }
        thread::sleep(Duration::from_millis(10));
    };

    assert_eq!(rows, ["STATUS-LINE".to_owned()]);
}

#[cfg(unix)]
#[test]
fn cursor_up_rewrite_publishes_a_single_row() {
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "printf 'Thinking (1s)\\n'; sleep 0.05; printf '\\033[1A\\r\\033[2KThinking (2s)\\n'"
                    .to_owned(),
            ],
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(geometry(40, 4), config, Arc::new(|| {}))
        .expect("terminal starts");

    for _ in 0..100 {
        if session.child_exited().expect("child status reads") {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let rows = loop {
        let rows = session
            .extract_frame()
            .expect("frame reads")
            .text_rows()
            .into_iter()
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>();
        if rows == ["Thinking (2s)".to_owned()] || Instant::now() > deadline {
            break rows;
        }
        thread::sleep(Duration::from_millis(10));
    };

    assert_eq!(rows, ["Thinking (2s)".to_owned()]);
}

#[cfg(unix)]
#[test]
fn streaming_rewrites_publish_coherent_frames() {
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "i=1; while [ $i -le 40 ]; do printf '\\033[2;1HThinking (%s)\\033[K' \"$i\"; i=$((i + 1)); sleep 0.05; done".to_owned(),
            ],
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(geometry(40, 6), config, Arc::new(|| {}))
        .expect("terminal starts");

    // Read frames while the child streams, like the live UI does. Every observed
    // frame must show a single spinner state; mixed rows mean a torn publish.
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    let mut saw_frames = 0;
    while Instant::now() < deadline {
        let rows = session.extract_frame().expect("frame reads").text_rows();
        saw_frames += 1;
        let spinners = rows
            .iter()
            .filter(|row| row.contains("Thinking"))
            .collect::<Vec<_>>();
        assert!(
            spinners.len() <= 1,
            "torn frame shows multiple spinner states: {spinners:?}"
        );
        if let [only] = spinners.as_slice() {
            assert!(
                only.starts_with("Thinking (") && only.ends_with(')'),
                "torn spinner row: {only:?}"
            );
        }
        if session.child_exited().expect("child status reads") {
            break;
        }
        thread::sleep(Duration::from_millis(20));
    }
    assert!(saw_frames > 3, "streaming starved frame publication");
}

#[cfg(unix)]
#[test]
fn continuous_plain_output_keeps_publishing_frames() {
    let repaint_count = Arc::new(AtomicUsize::new(0));
    let repaint_wakeup = {
        let repaint_count = Arc::clone(&repaint_count);
        Arc::new(move || {
            repaint_count.fetch_add(1, Ordering::Relaxed);
        })
    };
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "i=0; while [ $i -lt 40 ]; do printf 'frame-%s\\r' \"$i\"; i=$((i + 1)); sleep 0.002; done"
                    .to_owned(),
            ],
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(geometry(20, 4), config, repaint_wakeup)
        .expect("terminal starts");

    for _ in 0..100 {
        if session.child_exited().expect("child status reads") {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }

    assert!(
        repaint_count.load(Ordering::Relaxed) > 3,
        "continuous output starved frame publication"
    );
}

#[cfg(unix)]
#[test]
fn dropping_a_terminal_session_kills_its_owned_child() {
    let directory = assert_fs::TempDir::new().expect("pid directory");
    let pid_path = directory.path().join("child.pid");
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "echo $$ > \"$BOOTTY_TEST_PID\"; while :; do sleep 60; done".to_owned(),
            ],
            env: vec![(
                "BOOTTY_TEST_PID".to_owned(),
                pid_path.to_string_lossy().into_owned(),
            )],
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let session = TerminalSession::new_with_config(geometry(20, 4), config, Arc::new(|| {}))
        .expect("terminal starts");

    let pid = wait_for_pid(&pid_path).expect("child fixture ready");
    drop(session);

    for _ in 0..100 {
        if !process_alive(pid) {
            return;
        }
        thread::sleep(Duration::from_millis(10));
    }
    panic!("terminal child {pid} survived session drop");
}

#[cfg(unix)]
#[test]
fn negotiated_super_keys_use_the_configured_encoded_input_relay() {
    let directory = assert_fs::TempDir::new().expect("relay directory");
    let ready_path = directory.path().join("kitty-response");
    let (input_tx, input_rx) = std::sync::mpsc::channel();
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                r#"stty raw -echo; temporary_ready="$BOOTTY_TEST_READY.tmp.$$"; printf '\033[>7u\033[?u'; dd of="$temporary_ready" bs=5 count=1 2>/dev/null; mv "$temporary_ready" "$BOOTTY_TEST_READY"; sleep 60"#
                    .to_owned(),
            ],
            env: vec![(
                "BOOTTY_TEST_READY".to_owned(),
                ready_path.to_string_lossy().into_owned(),
            )],
            ..SessionLaunchConfig::default()
        },
        super_key_input_tx: Some(input_tx),
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(geometry(20, 4), config, Arc::new(|| {}))
        .expect("terminal starts");
    assert_eq!(
        wait_for_file(&ready_path).expect("child fixture ready"),
        "\x1b[?7u"
    );

    session
        .encode_key(KeyInput {
            key: TerminalKey::B,
            mods: KeyMods {
                alt: true,
                command: true,
                ..KeyMods::default()
            },
            repeat: false,
            utf8: Some("b"),
            unshifted: Some('b'),
        })
        .expect("key queues");

    assert_eq!(
        input_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("encoded key reaches relay"),
        b"\x1b[98;11u"
    );
}

#[cfg(unix)]
#[test]
fn resize_updates_grid_size_only_after_worker_result() {
    let initial = geometry(20, 8);
    let resized = TerminalGeometry {
        cols: 24,
        rows: 10,
        ..initial
    };
    let invalid = TerminalGeometry {
        cols: initial.cols,
        rows: 0,
        ..initial
    };
    let mut session = TerminalSession::new_with_repaint_wakeup(initial, Arc::new(|| {}))
        .expect("terminal starts");

    session.resize(resized).expect("terminal resizes");
    assert_eq!(session.grid_size(), (resized.cols, resized.rows));
    let frame = session.extract_frame().expect("resized frame");
    assert_eq!((frame.cols, frame.rows), (resized.cols, resized.rows));
    assert!(session.resize(invalid).is_err());
    assert_eq!(session.grid_size(), (resized.cols, resized.rows));
}

#[cfg(unix)]
#[rstest::rstest]
fn frame_source_updates_do_not_wait_for_worker_publication() {
    let initial = geometry(20, 8);
    let queued = TerminalGeometry {
        cols: 28,
        rows: 12,
        ..initial
    };
    let (published_tx, published_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let release_rx = std::sync::Mutex::new(Some(release_rx));
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".into()),
            args: vec!["-c".into(), "printf ready; read _".into()],
            shell_integration: false,
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(
        initial,
        config,
        Arc::new(move || {
            let _ = published_tx.send(());
            let release = release_rx.lock().unwrap().take();
            if let Some(release) = release {
                let _ = release.recv();
            }
        }),
    )
    .expect("terminal starts");
    published_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("initial publication");
    let previous = session.extract_frame().expect("initial frame");
    let metrics =
        TerminalFrameSource::set_render_cell_metrics(&mut session, CellMetrics::new(9.0, 18.0));
    let resize = TerminalFrameSource::resize(&mut session, queued);
    let pending = session.extract_frame().expect("pending frame");
    // Release before assertions so even a failure cannot strand the worker.
    let _ = release_tx.send(());

    metrics.expect("cell metrics enqueue while worker is paused");
    resize.expect("resize enqueues while worker is paused");
    assert!(Arc::ptr_eq(&pending, &previous));
    assert_eq!(session.grid_size(), (queued.cols, queued.rows));
    published_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("resized publication");
    let frame = TerminalFrameSource::extract_frame(&mut session).expect("resized frame");
    assert_eq!((frame.cols, frame.rows), (queued.cols, queued.rows));

    let invalid = TerminalGeometry { rows: 0, ..queued };
    assert!(TerminalFrameSource::resize(&mut session, invalid).is_err());
    assert_eq!(session.grid_size(), (queued.cols, queued.rows));
}

#[cfg(unix)]
#[test]
fn terminal_worker_applies_the_latest_absolute_scroll_target() {
    let (repaint_tx, repaint_rx) = std::sync::mpsc::channel();
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "i=0; while [ $i -lt 24 ]; do printf 'row-%02d\\r\\n' $i; i=$((i + 1)); done; read _"
                    .to_owned(),
            ],
            ..SessionLaunchConfig::default()
        },
        max_scrollback: 256 * 1024,
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(
        geometry(20, 4),
        config,
        Arc::new(move || {
            let _ = repaint_tx.send(());
        }),
    )
    .expect("terminal starts");

    let initial_deadline = Instant::now() + Duration::from_secs(1);
    let initial_frame_ready = loop {
        let remaining = initial_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break false;
        }
        if repaint_rx.recv_timeout(remaining).is_err() {
            break false;
        }
        let frame = session.extract_frame().expect("frame extracts");
        if frame
            .scrollbar
            .is_some_and(|scrollbar| scrollbar.offset > 4)
            && frame.text_rows().iter().any(|row| row.contains("row-23"))
        {
            break true;
        }
    };
    assert!(
        initial_frame_ready,
        "output should publish a frame containing the final marker row"
    );

    // Queue several targets before the worker necessarily publishes a frame. The final target
    // must be interpreted against the worker's live terminal, not the caller's stale frame.
    session
        .scroll_viewport_to(1)
        .expect("first absolute target queues");
    session
        .scroll_viewport_to(7)
        .expect("second absolute target queues");
    session
        .scroll_viewport_to(3)
        .expect("final absolute target queues");
    session
        .scroll_viewport_to(3)
        .expect("repeated absolute target queues");

    let target_deadline = Instant::now() + Duration::from_secs(1);
    let target_applied = loop {
        let remaining = target_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break false;
        }
        if repaint_rx.recv_timeout(remaining).is_err() {
            break false;
        }
        if session
            .extract_frame()
            .expect("frame extracts")
            .scrollbar
            .is_some_and(|scrollbar| scrollbar.offset == 3)
        {
            break true;
        }
    };
    assert!(
        target_applied,
        "absolute targets should publish the final frame"
    );
    assert_eq!(
        session
            .extract_frame()
            .expect("frame extracts")
            .scrollbar
            .map(|scrollbar| scrollbar.offset),
        Some(3)
    );
}

#[cfg(unix)]
#[test]
fn terminal_worker_response_operations_preserve_public_results() {
    let geometry = geometry(20, 8);
    let mut session = TerminalSession::new_with_repaint_wakeup(geometry, Arc::new(|| {}))
        .expect("terminal starts");

    session.enter_copy_mode().expect("copy mode starts");
    assert!(session.copy_mode_active().expect("copy mode state reads"));
    let outcome = session
        .handle_copy_mode_action(TerminalCopyModeAction::Cancel)
        .expect("copy mode action completes");
    assert!(!outcome.active);
    assert_eq!(
        session
            .format_selection(TerminalSelectionFormat::PlainText)
            .expect("selection formatting completes"),
        None
    );
    assert!(
        !session
            .search_viewport("", TerminalSearchDirection::Current)
            .expect("search completes")
    );
    session
        .is_mouse_tracking()
        .expect("mouse tracking state reads");
    session
        .discard_pending_output()
        .expect("pending output discard completes");
}

#[cfg(unix)]
#[test]
fn terminal_launch_applies_one_managed_environment_and_process_policy() {
    let directory = assert_fs::TempDir::new().expect("launch directory");
    let working_directory = directory.path().join("working");
    fs::create_dir(&working_directory).expect("working directory");
    let output_path = directory.path().join("launch.txt");
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell_integration: false,
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "temporary_output=${BOOTTY_TEST_OUTPUT}.tmp.$$; printf '%s' \"$TERM|$COLORTERM|$TERM_PROGRAM|$TERM_PROGRAM_VERSION|${TERMINFO-unset}|${REMOVE_ME-unset}|$PWD|$1|$BOOTTY_PANE\" > \"$temporary_output\" && mv \"$temporary_output\" \"$BOOTTY_TEST_OUTPUT\""
                    .to_owned(),
                "bootty-runtime-test".to_owned(),
                "argument".to_owned(),
            ],
            working_directory: Some(working_directory.clone()),
            pane_id: Some("%7".to_owned()),
            env: vec![
                (
                    "BOOTTY_TEST_OUTPUT".to_owned(),
                    output_path.to_string_lossy().into_owned(),
                ),
                ("TERM".to_owned(), "wrong".to_owned()),
                ("COLORTERM".to_owned(), "wrong".to_owned()),
                ("TERM_PROGRAM".to_owned(), "wrong".to_owned()),
                ("TERM_PROGRAM_VERSION".to_owned(), "wrong".to_owned()),
                ("BOOTTY_PANE".to_owned(), "wrong".to_owned()),
                ("TERMINFO".to_owned(), "wrong".to_owned()),
                ("REMOVE_ME".to_owned(), "present".to_owned()),
            ],
            env_remove: vec!["REMOVE_ME".to_owned()],
            term: "bootty-runtime-term".to_owned(),
            colorterm: "bootty-runtime-color".to_owned(),
            term_program: Some("Ghostty".to_owned()),
        },
        ..TerminalSessionConfig::default()
    };
    let _session = TerminalSession::new_with_config(geometry(20, 4), config, Arc::new(|| {}))
        .expect("terminal starts");

    let output = wait_for_file(&output_path).expect("child fixture ready");
    let fields = output.split('|').collect::<Vec<_>>();
    assert_eq!(fields.len(), 9, "launch output: {output}");
    assert_eq!(fields[0], "bootty-runtime-term");
    assert_eq!(fields[1], "bootty-runtime-color");
    assert_eq!(fields[2], "Ghostty");
    assert_eq!(fields[3], TERMINAL_PROGRAM_VERSION);
    assert_ne!(fields[4], "wrong");
    assert_eq!(fields[5], "unset");
    assert_eq!(
        fields[6],
        working_directory
            .canonicalize()
            .expect("canonical working directory")
            .to_string_lossy()
    );
    assert_eq!(fields[7], "argument");
    assert_eq!(fields[8], "%7");
}

#[cfg(unix)]
#[test]
fn terminal_launch_reports_initial_host_cell_metrics() {
    let directory = assert_fs::TempDir::new().expect("launch directory");
    let output_path = directory.path().join("cell-size.txt");
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/bash".to_owned()),
            args: vec![
                "-c".to_owned(),
                "printf '\\033[16t'; IFS= read -r -d t reply; printf '%st' \"$reply\" > \"$BOOTTY_TEST_OUTPUT\""
                    .to_owned(),
            ],
            env: vec![(
                "BOOTTY_TEST_OUTPUT".to_owned(),
                output_path.to_string_lossy().into_owned(),
            )],
            ..SessionLaunchConfig::default()
        },
        ..TerminalSessionConfig::default()
    };
    let _session = TerminalSession::new_with_config_and_host_metrics(
        geometry(20, 4),
        2.0,
        CellMetrics::new(11.5, 17.5),
        config,
        Arc::new(|| {}),
    )
    .expect("terminal starts");

    assert_eq!(
        wait_for_file(&output_path)
            .expect("child fixture ready")
            .as_bytes(),
        b"\x1b[6;35;23t"
    );
}

#[cfg(unix)]
fn wait_for_pid(path: &std::path::Path) -> anyhow::Result<u32> {
    for _ in 0..100 {
        if let Ok(value) = fs::read_to_string(path)
            && let Ok(pid) = value.trim().parse()
        {
            return Ok(pid);
        }
        thread::sleep(Duration::from_millis(10));
    }
    anyhow::bail!("terminal child did not publish its pid");
}

#[cfg(unix)]
fn wait_for_file(path: &std::path::Path) -> anyhow::Result<String> {
    for _ in 0..100 {
        if let Ok(value) = fs::read_to_string(path) {
            return Ok(value);
        }
        thread::sleep(Duration::from_millis(10));
    }
    anyhow::bail!("terminal child did not publish {}", path.display());
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(unix)]
#[rstest::rstest]
fn sustained_output_preserves_the_tail_without_unbounded_worker_backlog() {
    let directory = TempDir::new().expect("flood directory");
    let payload = directory.child("flood.vt");
    payload
        .write_binary(&b"\x1b[38;2;123;45;67mX".repeat(1_000_000))
        .expect("flood fixture");
    let (repaint_tx, repaint_rx) = std::sync::mpsc::channel();
    let config = TerminalSessionConfig {
        launch: SessionLaunchConfig {
            shell: Some("/bin/sh".to_owned()),
            args: vec![
                "-c".to_owned(),
                "cat \"$1\"; printf '\\033[0m\\033[2J\\033[Hflood-complete'; read _".to_owned(),
                "flood-test".to_owned(),
                payload.path().to_string_lossy().into_owned(),
            ],
            ..SessionLaunchConfig::default()
        },
        max_scrollback: 0,
        ..TerminalSessionConfig::default()
    };
    let mut session = TerminalSession::new_with_config(
        geometry(80, 24),
        config,
        Arc::new(move || {
            let _ = repaint_tx.send(());
        }),
    )
    .expect("flood terminal");
    let deadline = Instant::now()
        .checked_add(Duration::from_secs(5))
        .expect("test deadline");
    loop {
        repaint_rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("flood must finish");
        assert!(
            session.pending_pty_len() <= 1024 * 1024,
            "worker output must stay bounded independently of producer size"
        );
        if session
            .extract_frame()
            .expect("flood frame")
            .text_rows()
            .iter()
            .any(|row| row.contains("flood-complete"))
        {
            break;
        }
    }
}
