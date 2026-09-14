use std::{fmt::Write as _, hint::black_box};

use anyhow::{Context, Result};
use bootty_terminal::geometry::{TerminalGeometry, TerminalSurface};
use bootty_terminal::terminal_engine::TerminalEngine;
use bootty_ui::{
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};
use criterion::{BatchSize, Criterion};

#[derive(Clone)]
struct RecordedChunk {
    at_us: u64,
    bytes: Vec<u8>,
}

#[derive(Clone)]
struct ReplayFixture {
    name: &'static str,
    app: &'static str,
    version: &'static str,
    term: &'static str,
    cols: u16,
    rows: u16,
    chunks: Vec<RecordedChunk>,
}

#[derive(Clone, Copy)]
enum ReplaySpeed {
    RealTime,
    Double,
    TenX,
    AsFastAsPossible,
}

#[derive(Default)]
struct ReplayStats {
    chunks: usize,
    bytes: usize,
    virtual_replay_us: u64,
    visible_catch_up_us: u64,
    frame_interval_p50_us: u64,
    frame_interval_p95_us: u64,
    frame_interval_p99_us: u64,
    missed_frames: usize,
    final_hash: u64,
    commands: usize,
}

fn terminal_engine(cols: u16, rows: u16) -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
    .context("terminal engine")
}

fn surface_for(cols: u16, rows: u16) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::mul_add(f32::from(cols), 9.0, 20.0),
        f32::mul_add(f32::from(rows), 22.0, 20.0),
        bootty_terminal::geometry::CellMetrics::new(9.0, 22.0),
        bootty_terminal::geometry::TerminalPadding::uniform(10.0),
    )
}

fn push_chunk(
    chunks: &mut Vec<RecordedChunk>,
    at_us: &mut u64,
    cadence_us: u64,
    bytes: String,
) -> Result<()> {
    chunks.push(RecordedChunk {
        at_us: *at_us,
        bytes: bytes.into_bytes(),
    });
    *at_us = at_us
        .checked_add(cadence_us)
        .context("replay timestamp overflow")?;
    Ok(())
}

fn full_clear() -> String {
    "\x1b[?25l\x1b[H\x1b[0m\x1b[2J".to_owned()
}

fn neovim_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    push_chunk(&mut chunks, &mut at_us, 8_000, full_clear())?;
    for row in 1_usize..=38 {
        let color = 80_usize.saturating_add(row.rem_euclid(120));
        push_chunk(
            &mut chunks,
            &mut at_us,
            5_000,
            format!(
                "\x1b[{row};1H\x1b[38;5;{color}m{row:04}\x1b[0m \
                 fn replay_case_{row:02}(state: &mut TerminalState) {{ assert_eq!(state.tick, {row}); }}"
            ),
        )?;
    }
    for tick in 0_usize..96 {
        let row = 4_usize.saturating_add(tick.rem_euclid(28));
        push_chunk(
            &mut chunks,
            &mut at_us,
            12_000,
            format!("\x1b[{row};42H\x1b[48;5;24;38;5;231minsert tick {tick:03} 🥟\x1b[0m"),
        )?;
    }
    push_chunk(
        &mut chunks,
        &mut at_us,
        16_000,
        "\x1b[40;1H\x1b[48;5;236;38;5;252m NORMAL main.rs [+]  37,42  replay-bench \x1b[0m"
            .to_owned(),
    )?;
    Ok(ReplayFixture {
        name: "neovim_editing",
        app: "nvim",
        version: "synthetic-0.1",
        term: "xterm-ghostty",
        cols: 120,
        rows: 40,
        chunks,
    })
}

fn fzf_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    push_chunk(&mut chunks, &mut at_us, 4_000, full_clear())?;
    for page in 0_usize..24 {
        let mut bytes = format!(
            "\x1b[H\x1b[48;5;236;38;5;252m> query replay-{page:02}  {}/1000000 candidates\x1b[0m",
            page.checked_mul(512).context("replay arithmetic")?
        );
        for row in 2..=48 {
            let candidate = page
                .checked_mul(47)
                .and_then(|value| value.checked_add(row))
                .context("replay arithmetic")?;
            write!(bytes,
                "\x1b[{row};1H\x1b[38;5;{}m{:07}\x1b[0m crates/bootty/src/module_{:04}.rs  score={:03} {}",
                70_usize.saturating_add(row.rem_euclid(80)),
                candidate,
                candidate.rem_euclid(4096),
                candidate
                    .checked_mul(17)
                    .context("replay arithmetic")?
                    .rem_euclid(997),
                "match ".repeat(12)
            )
            .context("write replay fixture")?;
        }
        push_chunk(&mut chunks, &mut at_us, 10_000, bytes)?;
    }
    Ok(ReplayFixture {
        name: "fzf_million_candidates",
        app: "fzf",
        version: "synthetic-0.1",
        term: "xterm-256color",
        cols: 160,
        rows: 48,
        chunks,
    })
}

fn git_diff_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    push_chunk(&mut chunks, &mut at_us, 5_000, full_clear())?;
    for file in 0..18 {
        let mut bytes = format!(
            "\x1b[1;33mdiff --git a/crates/bootty/file_{file:03}.rs b/crates/bootty/file_{file:03}.rs\x1b[0m\r\n\x1b[36m@@ -1,40 +1,44 @@\x1b[0m\r\n"
        );
        for line in 0_usize..42 {
            let prefix = match line.rem_euclid(4) {
                0 => ("\x1b[32m+", " added optimized replay branch"),
                1 => ("\x1b[31m-", " removed redundant allocation"),
                _ => (" ", " unchanged context for benchmark replay"),
            };
            write!(
                bytes,
                "{}{:04}: {} {}\x1b[0m\r\n",
                prefix.0,
                line,
                prefix.1,
                "code ".repeat(14)
            )
            .context("write replay fixture")?;
        }
        push_chunk(&mut chunks, &mut at_us, 18_000, bytes)?;
    }
    Ok(ReplayFixture {
        name: "git_diff_10mb",
        app: "git",
        version: "synthetic-0.1",
        term: "xterm-256color",
        cols: 140,
        rows: 50,
        chunks,
    })
}

fn cargo_build_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    for batch in 0_usize..64 {
        let mut bytes = Vec::with_capacity(4096);
        for item in 0_usize..24 {
            let crate_id = batch
                .checked_mul(24)
                .and_then(|value| value.checked_add(item))
                .context("replay arithmetic")?;
            bytes.extend_from_slice(
                format!(
                    "\x1b[32m   Compiling\x1b[0m bootty-crate-{crate_id:04} v0.0.0 (/repo/crates/{crate_id:04})\r\n"
                )
                .as_bytes(),
            );
            if item.rem_euclid(7) == 0 {
                bytes.extend_from_slice(
                    format!(
                        "\x1b[33mwarning\x1b[0m: replay warning {crate_id:04}: {}\r\n",
                        "note ".repeat(18)
                    )
                    .as_bytes(),
                );
            }
        }
        chunks.push(RecordedChunk { at_us, bytes });
        at_us = at_us
            .checked_add(12_000)
            .context("replay timestamp overflow")?;
    }
    Ok(ReplayFixture {
        name: "cargo_build_log",
        app: "cargo",
        version: "synthetic-0.1",
        term: "xterm-256color",
        cols: 132,
        rows: 44,
        chunks,
    })
}

fn kubectl_logs_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    for batch in 0_usize..96 {
        let mut bytes = Vec::with_capacity(4096);
        for line in 0..16 {
            let id = batch
                .checked_mul(16)
                .and_then(|value| value.checked_add(line))
                .context("replay arithmetic")?;
            bytes.extend_from_slice(
                format!(
                    "2026-06-15T12:{:02}:{:02}.{:03}Z pod/api-{id:04} level={} trace={} {}\r\n",
                    id.rem_euclid(60),
                    id.checked_mul(7)
                        .context("replay arithmetic")?
                        .rem_euclid(60),
                    id.checked_mul(37)
                        .context("replay arithmetic")?
                        .rem_euclid(1000),
                    ["INFO", "DEBUG", "WARN", "ERROR"]
                        .get(id.rem_euclid(4))
                        .context("log level fixture")?,
                    id.checked_mul(17).context("replay arithmetic")?,
                    "json={\"event\":\"stream\",\"payload\":\"xxxxxxxx\"} ".repeat(3)
                )
                .as_bytes(),
            );
        }
        chunks.push(RecordedChunk { at_us, bytes });
        at_us = at_us
            .checked_add(4_000)
            .context("replay timestamp overflow")?;
    }
    Ok(ReplayFixture {
        name: "kubectl_logs_tail",
        app: "kubectl",
        version: "synthetic-0.1",
        term: "xterm-256color",
        cols: 180,
        rows: 60,
        chunks,
    })
}

fn btop_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    for tick in 0_usize..120 {
        let mut bytes = full_clear();
        write!(bytes,
            "\x1b[1;1H\x1b[48;5;24;38;5;231m btop replay tick {tick:03} cpu {:02}% mem {:02}% \x1b[0m",
            tick.checked_mul(3).context("replay arithmetic")?.rem_euclid(100),
            tick.checked_mul(7).context("replay arithmetic")?.rem_euclid(100)
        )
        .context("write replay fixture")?;
        for row in 3_usize..=42 {
            let pct = tick.saturating_add(row).rem_euclid(100);
            write!(bytes,
                "\x1b[{row};1H\x1b[38;5;{}mproc-{row:02}\x1b[0m pid={:05} cpu={pct:02}% [{}] net={}MB/s",
                80_usize.saturating_add(row.rem_euclid(100)),
                10_000_usize.saturating_add(row),
                "█".repeat(pct / 5),
                tick.checked_mul(row).context("replay arithmetic")?.rem_euclid(900)
            )
            .context("write replay fixture")?;
        }
        push_chunk(&mut chunks, &mut at_us, 16_666, bytes)?;
    }
    Ok(ReplayFixture {
        name: "btop_dashboard_60hz",
        app: "btop",
        version: "synthetic-0.1",
        term: "xterm-256color",
        cols: 150,
        rows: 46,
        chunks,
    })
}

fn tmux_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    for tick in 0_usize..72 {
        let mut bytes = full_clear();
        bytes.push_str("\x1b[1;1H\x1b[38;5;81m┌──────────────────────────────── tmux replay ────────────────────────────────┐\x1b[0m");
        for row in 2_usize..=42 {
            let pane = if row < 18 {
                "editor"
            } else if row < 31 {
                "test"
            } else {
                "logs"
            };
            write!(
                bytes,
                "\x1b[{row};1H\x1b[38;5;{}m│ {pane:<6}\x1b[0m tick={tick:03} row={row:02} {}",
                90_usize.saturating_add(row.rem_euclid(80)),
                "pane-output ".repeat(8)
            )
            .context("write replay fixture")?;
            write!(bytes, "\x1b[{row};82H\x1b[38;5;60m┃\x1b[0m").context("write replay fixture")?;
        }
        write!(bytes,
            "\x1b[44;1H\x1b[48;5;236;38;5;252m[0] zsh* [1] nvim [2] cargo [3] logs tick={tick:03}\x1b[0m"
        )
        .context("write replay fixture")?;
        push_chunk(&mut chunks, &mut at_us, 20_000, bytes)?;
    }
    Ok(ReplayFixture {
        name: "tmux_three_pane_session",
        app: "tmux",
        version: "synthetic-0.1",
        term: "screen-256color",
        cols: 170,
        rows: 44,
        chunks,
    })
}

fn ai_output_fixture() -> Result<ReplayFixture> {
    let mut chunks = Vec::new();
    let mut at_us = 0;
    for turn in 0_usize..80 {
        let mut bytes = format!(
            "\x1b[38;5;81massistant\x1b[0m turn={turn:03} tool={} status={}\r\n",
            ["read", "edit", "cargo", "sym", "review"]
                .get(turn.rem_euclid(5))
                .context("tool fixture")?,
            ["queued", "running", "done"]
                .get(turn.rem_euclid(3))
                .context("status fixture")?
        );
        for line in 0_usize..8 {
            let marker = match line.rem_euclid(3) {
                0 => "+",
                1 => "-",
                _ => "~",
            };
            write!(
                bytes,
                "\x1b[{}m{marker} generated code line {turn:03}.{line:02} {}\x1b[0m\r\n",
                if marker == "+" {
                    "32"
                } else if marker == "-" {
                    "31"
                } else {
                    "33"
                },
                "token ".repeat(18)
            )
            .context("write replay fixture")?;
        }
        push_chunk(&mut chunks, &mut at_us, 9_000, bytes)?;
    }
    Ok(ReplayFixture {
        name: "ai_codegen_stream",
        app: "ai-agent",
        version: "synthetic-0.1",
        term: "xterm-ghostty",
        cols: 160,
        rows: 60,
        chunks,
    })
}

fn fixtures() -> Result<Vec<ReplayFixture>> {
    Ok(vec![
        neovim_fixture()?,
        fzf_fixture()?,
        git_diff_fixture()?,
        cargo_build_fixture()?,
        kubectl_logs_fixture()?,
        btop_fixture()?,
        tmux_fixture()?,
        ai_output_fixture()?,
    ])
}

const fn speed_name(speed: ReplaySpeed) -> &'static str {
    match speed {
        ReplaySpeed::RealTime => "1x",
        ReplaySpeed::Double => "2x",
        ReplaySpeed::TenX => "10x",
        ReplaySpeed::AsFastAsPossible => "asap",
    }
}

const fn scaled_time(speed: ReplaySpeed, at_us: u64) -> u64 {
    match speed {
        ReplaySpeed::RealTime => at_us,
        ReplaySpeed::Double => at_us.div_euclid(2),
        ReplaySpeed::TenX => at_us.div_euclid(10),
        ReplaySpeed::AsFastAsPossible => 0,
    }
}

fn percentile(values: &mut [u64], numerator: usize, denominator: usize) -> Result<u64> {
    if values.is_empty() {
        return Ok(0);
    }
    values.sort_unstable();
    let index = values
        .len()
        .checked_sub(1)
        .context("percentile length")?
        .checked_mul(numerator)
        .context("percentile arithmetic")?
        .div_euclid(denominator);
    values.get(index).copied().context("percentile index")
}

fn hash_frame_text(text: &[char]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for ch in text {
        hash ^= u64::from(u32::from(*ch));
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn replay_fixture(
    fixture: &ReplayFixture,
    speed: ReplaySpeed,
    pipeline_every: usize,
) -> Result<ReplayStats> {
    let mut engine = terminal_engine(fixture.cols, fixture.rows)?;
    let mut planner = PaintPlanner::default();
    let surface = surface_for(fixture.cols, fixture.rows);
    let mut stats = ReplayStats {
        chunks: fixture.chunks.len(),
        ..ReplayStats::default()
    };
    let mut frame_intervals = Vec::with_capacity(fixture.chunks.len().saturating_sub(1));
    let mut previous_time = None;

    for (index, chunk) in fixture.chunks.iter().enumerate() {
        let virtual_time = scaled_time(speed, chunk.at_us);
        if let Some(previous) = previous_time {
            let interval = virtual_time.saturating_sub(previous);
            frame_intervals.push(interval);
            if interval > 16_666 {
                let missed = usize::try_from(interval.div_euclid(16_666).saturating_sub(1))
                    .context("missed frame count conversion")?;
                stats.missed_frames = stats
                    .missed_frames
                    .checked_add(missed)
                    .context("missed frame count overflow")?;
            }
        }
        previous_time = Some(virtual_time);
        engine.write_vt(&chunk.bytes);
        stats.bytes = stats
            .bytes
            .checked_add(chunk.bytes.len())
            .context("replay byte count overflow")?;

        if pipeline_every != 0 && index.rem_euclid(pipeline_every) == 0 {
            let frame = engine.extract_frame().context("replay frame")?;
            let plan = planner.plan(surface, frame, 16.0).clone();
            let text_contract = TerminalTextContract::for_terminal_paint_plan(
                &plan,
                &TerminalTextConfig::default(),
            );
            stats.commands = stats
                .commands
                .checked_add(
                    TerminalRenderFrame::from_plan(&plan, &text_contract)
                        .commands
                        .len(),
                )
                .context("replay command count overflow")?;
        }
    }

    let final_frame = engine.extract_frame().context("final replay frame")?;
    let final_plan = planner.plan(surface, final_frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&final_plan, &TerminalTextConfig::default());
    stats.commands = stats
        .commands
        .checked_add(
            TerminalRenderFrame::from_plan(&final_plan, &text_contract)
                .commands
                .len(),
        )
        .context("replay command count overflow")?;
    stats.final_hash = hash_frame_text(&final_frame.text);
    stats.virtual_replay_us = fixture
        .chunks
        .last()
        .map(|chunk| scaled_time(speed, chunk.at_us))
        .unwrap_or_default();
    stats.visible_catch_up_us = stats.virtual_replay_us.saturating_sub(
        fixture
            .chunks
            .iter()
            .rev()
            .find(|chunk| !chunk.bytes.is_empty())
            .map(|chunk| scaled_time(speed, chunk.at_us))
            .unwrap_or_default(),
    );
    let mut p50 = frame_intervals.clone();
    let mut p95 = frame_intervals.clone();
    let mut p99 = frame_intervals;
    stats.frame_interval_p50_us = percentile(&mut p50, 50, 100)?;
    stats.frame_interval_p95_us = percentile(&mut p95, 95, 100)?;
    stats.frame_interval_p99_us = percentile(&mut p99, 99, 100)?;
    black_box((
        fixture.app,
        fixture.version,
        fixture.term,
        stats.chunks,
        stats.bytes,
        stats.final_hash,
        stats.commands,
    ));
    Ok(stats)
}

fn bench_replay_asap(c: &mut Criterion) -> Result<()> {
    for fixture in fixtures()? {
        let mut failure = None;
        c.bench_function(&format!("real_app_replay_{}_asap", fixture.name), |b| {
            b.iter_batched(
                || fixture.clone(),
                |fixture| match replay_fixture(&fixture, ReplaySpeed::AsFastAsPossible, 0) {
                    Ok(value) => {
                        black_box(value);
                    }
                    Err(error) => failure = Some(error),
                },
                BatchSize::SmallInput,
            );
        });
        failure.map_or(Ok(()), Err)?;
    }
    Ok(())
}

fn bench_replay_scaled(c: &mut Criterion) -> Result<()> {
    for fixture in fixtures()? {
        for speed in [
            ReplaySpeed::RealTime,
            ReplaySpeed::Double,
            ReplaySpeed::TenX,
        ] {
            let mut failure = None;
            c.bench_function(
                &format!(
                    "real_app_replay_{}_{}_pipeline",
                    fixture.name,
                    speed_name(speed)
                ),
                |b| {
                    b.iter_batched(
                        || fixture.clone(),
                        |fixture| match replay_fixture(&fixture, speed, 8) {
                            Ok(value) => {
                                black_box(value);
                            }
                            Err(error) => failure = Some(error),
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
            failure.map_or(Ok(()), Err)?;
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .noise_threshold(0.15)
        .configure_from_args();
    bench_replay_asap(&mut criterion)?;
    bench_replay_scaled(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
