use std::{hint::black_box, time::Duration};

use anyhow::{Result, ensure};
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface};
use bootty_terminal::terminal_engine::{TerminalColorConfig, TerminalEngine};
use bootty_ui::{
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};
use criterion::{BatchSize, Criterion};

const GEOMETRY: TerminalGeometry = TerminalGeometry {
    cols: 160,
    rows: 48,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Clone, Copy)]
enum UpdatePattern {
    CursorOnly,
    SingleCell,
    StatusLine,
    OneRow,
    OneColumn,
    Random5Pct,
    Random25Pct,
    FullRepaint,
    ScrollOneLine,
    ScrollHalfScreen,
    ScrollFullScreen,
    AlternateScreenRedraw,
    ScrollbackAppend,
}

impl UpdatePattern {
    const fn name(self) -> &'static str {
        match self {
            Self::CursorOnly => "cursor_only",
            Self::SingleCell => "single_cell",
            Self::StatusLine => "statusline",
            Self::OneRow => "one_row",
            Self::OneColumn => "one_column",
            Self::Random5Pct => "random_5pct",
            Self::Random25Pct => "random_25pct",
            Self::FullRepaint => "full_repaint",
            Self::ScrollOneLine => "scroll_one_line",
            Self::ScrollHalfScreen => "scroll_half_screen",
            Self::ScrollFullScreen => "scroll_full_screen",
            Self::AlternateScreenRedraw => "alternate_screen_redraw",
            Self::ScrollbackAppend => "scrollback_append",
        }
    }
}

#[derive(Default)]
struct PacingStats {
    frames: usize,
    missed_budget: usize,
    max_frame_ns: u128,
    commands: usize,
    dirty_rows: usize,
    cells: usize,
    chars: usize,
    hash: u64,
}

impl PacingStats {
    fn checksum(&self) -> Result<u64> {
        Ok(self.hash
            ^ u64::try_from(self.frames)?
            ^ u64::try_from(self.missed_budget)?.rotate_left(7)
            ^ u64::try_from(self.max_frame_ns)?.rotate_left(13)
            ^ u64::try_from(self.commands)?.rotate_left(19)
            ^ u64::try_from(self.dirty_rows)?.rotate_left(29)
            ^ u64::try_from(self.cells)?.rotate_left(37)
            ^ u64::try_from(self.chars)?.rotate_left(43))
    }
}

fn surface_for(geometry: TerminalGeometry) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::mul_add(f32::from(geometry.cols), 9.0, 20.0),
        f32::mul_add(f32::from(geometry.rows), 22.0, 20.0),
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::uniform(10.0),
    )
}

fn seeded_engine() -> Result<TerminalEngine> {
    let mut engine =
        TerminalEngine::new_with_scrollback(GEOMETRY, TerminalColorConfig::default(), 32_000_000)?;
    for row in 1..=GEOMETRY.rows {
        engine.write_vt(
            format!(
                "\x1b[{row};1H\x1b[38;5;{}mseed row {row:03}\x1b[0m {}",
                16u16.saturating_add(row.rem_euclid(200)),
                "baseline cells ".repeat(12)
            )
            .as_bytes(),
        );
    }
    Ok(engine)
}

fn apply_pattern(engine: &mut TerminalEngine, pattern: UpdatePattern, tick: u32) -> Result<()> {
    match pattern {
        UpdatePattern::CursorOnly => {
            let row = 1u32.saturating_add(tick.rem_euclid(u32::from(GEOMETRY.rows)));
            let col =
                1u32.saturating_add(tick.saturating_mul(7).rem_euclid(u32::from(GEOMETRY.cols)));
            engine.write_vt(format!("\x1b[{row};{col}H").as_bytes());
        }
        UpdatePattern::SingleCell => {
            let row = 1u32.saturating_add(tick.rem_euclid(u32::from(GEOMETRY.rows)));
            let col =
                1u32.saturating_add(tick.saturating_mul(13).rem_euclid(u32::from(GEOMETRY.cols)));
            let ch = char::from(b'a'.saturating_add(u8::try_from(tick.rem_euclid(26))?));
            engine.write_vt(format!("\x1b[{row};{col}H{ch}").as_bytes());
        }
        UpdatePattern::StatusLine => {
            engine.write_vt(
                format!(
                    "\x1b[1;1H\x1b[48;5;236;38;5;81m frame {tick:06} {}\x1b[0m",
                    "status ".repeat(24)
                )
                .as_bytes(),
            );
        }
        UpdatePattern::OneRow => {
            let row = 1u32.saturating_add(tick.rem_euclid(u32::from(GEOMETRY.rows)));
            engine.write_vt(
                format!(
                    "\x1b[{row};1H\x1b[38;2;{};{};220mrow update {tick:06} {}\x1b[0m",
                    tick.rem_euclid(255),
                    tick.saturating_mul(3).rem_euclid(255),
                    "row cells ".repeat(20)
                )
                .as_bytes(),
            );
        }
        UpdatePattern::OneColumn => {
            let col = 1u32.saturating_add(tick.rem_euclid(u32::from(GEOMETRY.cols)));
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(format!("\x1b[{row};{col}H┃").as_bytes());
            }
        }
        UpdatePattern::Random5Pct => random_cells(engine, tick, 384)?,
        UpdatePattern::Random25Pct => random_cells(engine, tick, 1_920)?,
        UpdatePattern::FullRepaint => full_repaint(engine, tick),
        UpdatePattern::ScrollOneLine => {
            engine
                .write_vt(format!("\r\nscroll one {tick:06} {}", "payload ".repeat(18)).as_bytes());
        }
        UpdatePattern::ScrollHalfScreen => {
            for row in 0..GEOMETRY.rows.saturating_div(2) {
                engine.write_vt(
                    format!(
                        "\r\nscroll half {tick:06}-{row:02} {}",
                        "payload ".repeat(18)
                    )
                    .as_bytes(),
                );
            }
        }
        UpdatePattern::ScrollFullScreen => {
            for row in 0..GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\r\nscroll full {tick:06}-{row:02} {}",
                        "payload ".repeat(18)
                    )
                    .as_bytes(),
                );
            }
        }
        UpdatePattern::AlternateScreenRedraw => {
            engine.write_vt(b"\x1b[?1049h\x1b[2J\x1b[H");
            full_repaint(engine, tick);
            engine.write_vt(b"\x1b[?1049l");
        }
        UpdatePattern::ScrollbackAppend => {
            for row in 0..8 {
                engine.write_vt(
                    format!(
                        "\r\nscrollback append {tick:06}-{row:02} {}",
                        "history ".repeat(22)
                    )
                    .as_bytes(),
                );
            }
        }
    }
    Ok(())
}

fn random_cells(engine: &mut TerminalEngine, tick: u32, cells: usize) -> Result<()> {
    let mut seed = tick.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
    for index in 0..cells {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let row = 1u32.saturating_add(seed.rem_euclid(u32::from(GEOMETRY.rows)));
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let col = 1u32.saturating_add(seed.rem_euclid(u32::from(GEOMETRY.cols)));
        let ch = char::from(b'a'.saturating_add(u8::try_from(
            (usize::try_from(tick)?.saturating_add(index)).rem_euclid(26),
        )?));
        engine.write_vt(format!("\x1b[{row};{col}H{ch}").as_bytes());
    }
    Ok(())
}

fn full_repaint(engine: &mut TerminalEngine, tick: u32) {
    engine.write_vt(b"\x1b[H");
    for row in 1..=GEOMETRY.rows {
        engine.write_vt(
            format!(
                "\x1b[{row};1H\x1b[38;2;{};{};230mfull {tick:06} row {row:03}\x1b[0m {}",
                tick.saturating_add(u32::from(row)).rem_euclid(255),
                tick.saturating_mul(3)
                    .saturating_add(u32::from(row))
                    .rem_euclid(255),
                "frame cells ".repeat(16)
            )
            .as_bytes(),
        );
    }
}

fn hash_text(text: &[char]) -> u64 {
    text.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
        (hash ^ u64::from(u32::from(*ch))).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn run_pacing(pattern: UpdatePattern, target_hz: u32, frames: u32) -> Result<u64> {
    let mut engine = seeded_engine()?;
    let mut planner = PaintPlanner::default();
    let surface = surface_for(GEOMETRY);
    let budget = Duration::from_secs_f64(f64::from(target_hz).recip());
    let mut stats = PacingStats::default();

    for tick in 0..frames {
        let start = std::time::Instant::now();
        apply_pattern(&mut engine, pattern, tick)?;
        let frame = engine.extract_frame()?;
        let plan = planner.plan(surface, frame, 16.0).clone();
        let text_contract =
            TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
        let render_frame = TerminalRenderFrame::from_plan(&plan, &text_contract);
        let elapsed = start.elapsed();

        stats.frames = stats.frames.saturating_add(1);
        stats.missed_budget = stats
            .missed_budget
            .saturating_add(usize::from(elapsed > budget));
        stats.max_frame_ns = stats.max_frame_ns.max(elapsed.as_nanos());
        stats.commands = stats.commands.saturating_add(render_frame.commands.len());
        stats.dirty_rows = stats.dirty_rows.saturating_add(frame.stats.dirty_rows);
        stats.cells = stats.cells.saturating_add(frame.stats.cells);
        stats.chars = stats.chars.saturating_add(frame.stats.chars);
        stats.hash ^= hash_text(&frame.text);
    }

    ensure!(
        stats.frames == usize::try_from(frames)?,
        "pacing frame count mismatch"
    );
    stats.checksum()
}

const fn core_patterns() -> [UpdatePattern; 13] {
    [
        UpdatePattern::CursorOnly,
        UpdatePattern::SingleCell,
        UpdatePattern::StatusLine,
        UpdatePattern::OneRow,
        UpdatePattern::OneColumn,
        UpdatePattern::Random5Pct,
        UpdatePattern::Random25Pct,
        UpdatePattern::FullRepaint,
        UpdatePattern::ScrollOneLine,
        UpdatePattern::ScrollHalfScreen,
        UpdatePattern::ScrollFullScreen,
        UpdatePattern::AlternateScreenRedraw,
        UpdatePattern::ScrollbackAppend,
    ]
}

fn bench_core_patterns(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    for pattern in core_patterns() {
        c.bench_function(&format!("render_pacing_{}_120hz", pattern.name()), |b| {
            b.iter_batched(
                || pattern,
                |pattern| {
                    black_box(run_pacing(pattern, 120, 32).map_err(|error| failure = Some(error)))
                },
                BatchSize::SmallInput,
            );
        });
    }
    failure.map_or(Ok(()), Err)
}

fn bench_refresh_targets(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    for target_hz in [30, 60, 120, 144, 240] {
        for pattern in [UpdatePattern::Random25Pct, UpdatePattern::FullRepaint] {
            c.bench_function(
                &format!("render_refresh_target_{}_{}hz", pattern.name(), target_hz),
                |b| {
                    b.iter_batched(
                        || pattern,
                        |pattern| {
                            black_box(
                                run_pacing(pattern, target_hz, 32)
                                    .map_err(|error| failure = Some(error)),
                            )
                        },
                        BatchSize::SmallInput,
                    );
                },
            );
        }
    }
    failure.map_or(Ok(()), Err)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .sample_size(10)
        .noise_threshold(0.20)
        .configure_from_args();
    bench_core_patterns(&mut criterion)?;
    bench_refresh_targets(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
