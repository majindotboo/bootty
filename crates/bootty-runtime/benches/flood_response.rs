use std::{
    hint::black_box,
    sync::Arc,
    thread,
    time::{Duration, Instant},
};

use bootty_runtime::{
    PtyBacklog, TerminalSession, TerminalSessionConfig, drain_pty_backlog,
    geometry::TerminalGeometry, terminal_session::SessionLaunchConfig,
};
use bootty_terminal::terminal_engine::TerminalEngine;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};

const CTRL_C: &[u8] = b"\x03";
const LIVE_FLOOD_WARMUP: Duration = Duration::from_millis(15);
const LIVE_FLOOD_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
struct FloodFixture {
    name: &'static str,
    payload: Vec<u8>,
    chunk_size: usize,
}

#[derive(Default)]
struct FloodReplayStats {
    frames: usize,
    bytes: usize,
    high_water: usize,
    ctrl_c_visible_frame: usize,
    input_visible_frame: usize,
    scroll_frame: usize,
    drain_us: u64,
    final_text_len: usize,
}

#[derive(Default)]
struct LiveCtrlCStats {
    latency_us: u64,
    polls: usize,
    bytes: usize,
    high_water: usize,
    exited: bool,
}

fn geometry(cols: u16, rows: u16) -> TerminalGeometry {
    TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    }
}

fn terminal_engine() -> TerminalEngine {
    TerminalEngine::new(geometry(160, 48)).expect("terminal engine")
}

fn push_line(payload: &mut Vec<u8>, line: impl AsRef<str>) {
    payload.extend_from_slice(line.as_ref().as_bytes());
    payload.extend_from_slice(b"\r\n");
}

fn repeated_lines(lines: usize, mut line: impl FnMut(usize) -> String) -> Vec<u8> {
    let mut payload = Vec::with_capacity(lines * 96);
    for index in 0..lines {
        push_line(&mut payload, line(index));
    }
    payload
}

fn flood_fixtures() -> Vec<FloodFixture> {
    vec![
        FloodFixture {
            name: "flood_yes_replay",
            payload: repeated_lines(16_384, |_| "y".to_owned()),
            chunk_size: 8 * 1024,
        },
        FloodFixture {
            name: "flood_seq_replay",
            payload: repeated_lines(16_384, |index| index.to_string()),
            chunk_size: 8 * 1024,
        },
        FloodFixture {
            name: "flood_find_replay",
            payload: repeated_lines(12_000, |index| {
                format!("/repo/target/debug/build/bootty-{index:06}/out/generated/module.rs")
            }),
            chunk_size: 16 * 1024,
        },
        FloodFixture {
            name: "flood_journalctl_replay",
            payload: repeated_lines(8_192, |index| {
                format!(
                    "Jun 15 12:{:02}:{:02} host bootty[{index}]: level={} unit=bootty.service message='{}'",
                    index % 60,
                    (index * 7) % 60,
                    ["INFO", "DEBUG", "WARN", "ERR"][index % 4],
                    "event replay ".repeat(4)
                )
            }),
            chunk_size: 16 * 1024,
        },
        FloodFixture {
            name: "flood_docker_logs_replay",
            payload: repeated_lines(8_192, |index| {
                format!(
                    "container=api-{index:04} stream=stdout json={{\"level\":\"{}\",\"request\":{},\"payload\":\"{}\"}}",
                    ["info", "debug", "warn", "error"][index % 4],
                    index * 17,
                    "x".repeat(48)
                )
            }),
            chunk_size: 16 * 1024,
        },
        FloodFixture {
            name: "flood_kubectl_logs_replay",
            payload: repeated_lines(8_192, |index| {
                format!(
                    "2026-06-15T12:{:02}:{:02}.{:03}Z pod/api-{index:04} ns=prod level={} trace={} {}",
                    index % 60,
                    (index * 11) % 60,
                    (index * 37) % 1000,
                    ["INFO", "DEBUG", "WARN", "ERROR"][index % 4],
                    index * 23,
                    "field=value ".repeat(8)
                )
            }),
            chunk_size: 16 * 1024,
        },
        FloodFixture {
            name: "flood_cat_colored_log_replay",
            payload: repeated_lines(10_000, |index| {
                format!(
                    "\x1b[38;5;{}m{:06}: colored log row {}\x1b[0m {}",
                    16 + index % 200,
                    index,
                    ["ok", "warn", "error", "trace"][index % 4],
                    "payload ".repeat(10)
                )
            }),
            chunk_size: 16 * 1024,
        },
        FloodFixture {
            name: "flood_python_print_replay",
            payload: repeated_lines(12_000, |index| {
                format!("python print {index:06} {}", "x".repeat(96))
            }),
            chunk_size: 32 * 1024,
        },
        FloodFixture {
            name: "flood_ripgrep_replay",
            payload: repeated_lines(8_192, |index| {
                format!(
                    "crates/bootty/src/module_{:04}.rs:{}:{}: match {}",
                    index % 512,
                    index % 2000,
                    index % 120,
                    "needle ".repeat(14)
                )
            }),
            chunk_size: 16 * 1024,
        },
        FloodFixture {
            name: "flood_cargo_build_replay",
            payload: repeated_lines(8_192, |index| {
                format!(
                    "\x1b[32m   Compiling\x1b[0m bootty-crate-{index:04} v0.0.0 (/repo/crates/{index:04}) {}",
                    "note ".repeat(8)
                )
            }),
            chunk_size: 16 * 1024,
        },
        FloodFixture {
            name: "flood_npm_install_replay",
            payload: repeated_lines(8_192, |index| {
                format!(
                    "npm http fetch GET 200 https://registry.npmjs.org/pkg-{index:04} {}ms {}",
                    10 + index % 400,
                    "dep ".repeat(12)
                )
            }),
            chunk_size: 16 * 1024,
        },
    ]
}

fn backlog_from_payload(payload: &[u8], chunk_size: usize) -> PtyBacklog {
    let mut backlog = PtyBacklog::with_capacity(payload.len().div_ceil(chunk_size));
    for chunk in payload.chunks(chunk_size) {
        backlog.push_back(chunk.to_vec());
    }
    backlog
}

fn run_flood_replay(fixture: &FloodFixture) -> FloodReplayStats {
    let mut engine = terminal_engine();
    let mut backlog = backlog_from_payload(&fixture.payload, fixture.chunk_size);
    let mut stats = FloodReplayStats::default();
    let mut injected_ctrl_c = false;
    let mut injected_input = false;
    let mut injected_scroll = false;

    while !backlog.is_empty() {
        stats.high_water = stats.high_water.max(backlog.len());
        let drain = drain_pty_backlog(&mut backlog, |bytes| engine.write_vt(bytes));
        stats.frames += 1;
        stats.bytes += drain.bytes;
        stats.drain_us += drain.elapsed_us;

        if !injected_ctrl_c && stats.bytes >= fixture.payload.len() / 4 {
            engine.write_vt(b"^C\r\n");
            stats.ctrl_c_visible_frame = stats.frames;
            injected_ctrl_c = true;
        }
        if !injected_input && stats.bytes >= fixture.payload.len() / 2 {
            engine.write_vt(b"typed while flood active\r\n");
            stats.input_visible_frame = stats.frames;
            injected_input = true;
        }
        if !injected_scroll && stats.bytes >= fixture.payload.len() * 3 / 4 {
            engine.scroll_viewport_delta(-12);
            stats.scroll_frame = stats.frames;
            injected_scroll = true;
        }
    }

    let frame = engine.extract_frame().expect("final flood frame");
    stats.final_text_len = frame.text.len();
    stats
}

#[cfg(windows)]
fn flood_command() -> String {
    "for /L %i in () do @echo bootty flood ctrl-c %i payload payload payload".to_owned()
}

#[cfg(not(windows))]
fn flood_command() -> String {
    "i=0; while :; do printf 'bootty flood ctrl-c %08d payload payload payload payload\\n' \"$i\"; i=$((i+1)); done".to_owned()
}

fn shell_launch_config(command: String) -> SessionLaunchConfig {
    #[cfg(windows)]
    let (shell, args) = ("cmd.exe".to_owned(), vec!["/C".to_owned(), command]);
    #[cfg(not(windows))]
    let (shell, args) = ("/bin/sh".to_owned(), vec!["-c".to_owned(), command]);

    SessionLaunchConfig {
        shell: Some(shell),
        args,
        ..SessionLaunchConfig::default()
    }
}

fn live_ctrl_c_under_flood() -> LiveCtrlCStats {
    let config = TerminalSessionConfig {
        launch: shell_launch_config(flood_command()),
        ..TerminalSessionConfig::default()
    };
    let mut terminal = TerminalSession::new_with_config(geometry(120, 40), config, Arc::new(|| {}))
        .expect("spawn flood command");
    let warmup_started = Instant::now();
    let mut stats = LiveCtrlCStats::default();

    while warmup_started.elapsed() < LIVE_FLOOD_WARMUP {
        let drain = terminal.drain_pty();
        stats.bytes += drain.bytes;
        stats.high_water = stats.high_water.max(terminal.pending_pty_len());
        thread::sleep(Duration::from_millis(1));
    }

    let interrupt_started = Instant::now();
    terminal.write_input(CTRL_C).expect("send ctrl-c");
    loop {
        let drain = terminal.drain_pty();
        stats.polls += 1;
        stats.bytes += drain.bytes;
        stats.high_water = stats.high_water.max(terminal.pending_pty_len());
        if terminal.child_exited().unwrap_or(false) {
            stats.latency_us = interrupt_started.elapsed().as_micros() as u64;
            stats.exited = true;
            return stats;
        }
        assert!(
            interrupt_started.elapsed() < LIVE_FLOOD_TIMEOUT,
            "flood command did not exit after Ctrl-C"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn bench_flood_replays(c: &mut Criterion) {
    for fixture in flood_fixtures() {
        c.bench_function(fixture.name, |b| {
            b.iter_batched(
                || fixture.clone(),
                |fixture| black_box(run_flood_replay(&fixture)),
                BatchSize::SmallInput,
            )
        });
    }
}

fn bench_live_ctrl_c(c: &mut Criterion) {
    c.bench_function("flood_live_ctrl_c_to_child_exit", |b| {
        b.iter(|| black_box(live_ctrl_c_under_flood()))
    });
}

criterion_group!(
    name = benches;
    config = Criterion::default().noise_threshold(0.20).sample_size(10);
    targets = bench_flood_replays, bench_live_ctrl_c,
);
criterion_main!(benches);
