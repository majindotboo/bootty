use std::hint::black_box;

use anyhow::Result;
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface};
use bootty_terminal::terminal_engine::TerminalEngine;
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
enum PowerCase {
    IdlePrompt,
    TypingEightCps,
    ReadlineEditing,
    LessScrolling,
    NeovimEditing,
    StdoutFlood,
    DoomFireAnimation,
    DashboardAnimation,
    ImageAnimation,
    ManyTabsIdle,
    ManyPanesActive,
}

impl PowerCase {
    const fn name(self) -> &'static str {
        match self {
            Self::IdlePrompt => "idle_prompt",
            Self::TypingEightCps => "typing_8cps",
            Self::ReadlineEditing => "readline_editing",
            Self::LessScrolling => "less_scrolling",
            Self::NeovimEditing => "neovim_editing",
            Self::StdoutFlood => "stdout_flood",
            Self::DoomFireAnimation => "doom_fire_animation",
            Self::DashboardAnimation => "dashboard_animation",
            Self::ImageAnimation => "image_animation",
            Self::ManyTabsIdle => "many_tabs_idle",
            Self::ManyPanesActive => "many_panes_active",
        }
    }

    const fn modeled_instances(self) -> usize {
        match self {
            Self::ManyTabsIdle => 32,
            Self::ManyPanesActive => 16,
            _ => 1,
        }
    }

    const fn target_hz(self) -> u32 {
        match self {
            Self::IdlePrompt | Self::ManyTabsIdle => 1,
            Self::TypingEightCps => 8,
            Self::ReadlineEditing | Self::LessScrolling | Self::NeovimEditing => 30,
            Self::StdoutFlood
            | Self::DoomFireAnimation
            | Self::DashboardAnimation
            | Self::ImageAnimation
            | Self::ManyPanesActive => 60,
        }
    }
}

const CASES: [PowerCase; 11] = [
    PowerCase::IdlePrompt,
    PowerCase::TypingEightCps,
    PowerCase::ReadlineEditing,
    PowerCase::LessScrolling,
    PowerCase::NeovimEditing,
    PowerCase::StdoutFlood,
    PowerCase::DoomFireAnimation,
    PowerCase::DashboardAnimation,
    PowerCase::ImageAnimation,
    PowerCase::ManyTabsIdle,
    PowerCase::ManyPanesActive,
];

#[derive(Default)]
struct PowerStats {
    frames: usize,
    commands: usize,
    cells: usize,
    chars: usize,
    instances: usize,
    hash: u64,
}

impl PowerStats {
    fn checksum(&self) -> Result<u64> {
        Ok(self.hash
            ^ u64::try_from(self.frames)?
            ^ u64::try_from(self.commands)?.rotate_left(7)
            ^ u64::try_from(self.cells)?.rotate_left(13)
            ^ u64::try_from(self.chars)?.rotate_left(19)
            ^ u64::try_from(self.instances)?.rotate_left(29))
    }
}

fn terminal_engine() -> Result<TerminalEngine> {
    TerminalEngine::new(GEOMETRY)
}

fn surface() -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::mul_add(f32::from(GEOMETRY.cols), 9.0, 20.0),
        f32::mul_add(f32::from(GEOMETRY.rows), 22.0, 20.0),
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::uniform(10.0),
    )
}

fn write_case_frame(engine: &mut TerminalEngine, case: PowerCase, tick: u32) {
    match case {
        PowerCase::IdlePrompt | PowerCase::ManyTabsIdle => {
            if tick == 0 {
                engine.write_vt(b"$ ");
            }
        }
        PowerCase::TypingEightCps => {
            engine.write_vt(format!("\x1b[H$ typed-{tick:04}").as_bytes());
        }
        PowerCase::ReadlineEditing => {
            engine.write_vt(
                format!("\x1b[H$ cargo test --workspace --lib --tests # edit {tick:04}").as_bytes(),
            );
        }
        PowerCase::LessScrolling => {
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1Hless line {:06} {}",
                        tick.saturating_add(u32::from(row)),
                        "doc ".repeat(20)
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::NeovimEditing => {
            engine.write_vt(b"\x1b[H\x1b[48;5;236;38;5;252m NORMAL src/main.rs ");
            for row in 2..GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1H\x1b[38;5;{}m{:04} let value_{tick}_{row} = compute()?; // λ 🥟\x1b[0m",
                        70u16.saturating_add(row.rem_euclid(120)),
                        row
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::StdoutFlood => {
            for line in 0..128 {
                engine.write_vt(
                    format!(
                        "flood tick={tick:04} line={line:03} {}\r\n",
                        "payload ".repeat(8)
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::DoomFireAnimation => {
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1H\x1b[48;5;{}m{}\x1b[0m",
                        16u32.saturating_add(u32::from(row).saturating_add(tick).rem_euclid(200)),
                        " ".repeat(usize::from(GEOMETRY.cols))
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::DashboardAnimation | PowerCase::ManyPanesActive => {
            for row in 1..=GEOMETRY.rows {
                engine.write_vt(
                    format!(
                        "\x1b[{row};1H\x1b[38;2;{};{};{}m▌ cpu={:02}% mem={:04}M net={} tick={tick:04}\x1b[0m",
                        tick.saturating_add(u32::from(row).saturating_mul(3)).rem_euclid(255),
                        tick.saturating_mul(2).saturating_add(u32::from(row).saturating_mul(5)).rem_euclid(255),
                        tick.saturating_mul(3).saturating_add(u32::from(row).saturating_mul(7)).rem_euclid(255),
                        tick.saturating_add(u32::from(row)).rem_euclid(100),
                        256u16.saturating_add(row.saturating_mul(7)),
                        "▁▂▃▄▅▆▇█".repeat(8)
                    )
                    .as_bytes(),
                );
            }
        }
        PowerCase::ImageAnimation => {
            engine.write_vt(
                format!(
                    "\x1b[Himage animation frame {tick:04} {}\r\n",
                    "▀▄█▌▐░▒▓".repeat(24)
                )
                .as_bytes(),
            );
        }
    }
}

fn render_once(
    engine: &mut TerminalEngine,
    planner: &mut PaintPlanner,
    stats: &mut PowerStats,
) -> Result<()> {
    let frame = engine.extract_frame()?;
    let plan = planner.plan(surface(), frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    let render_frame =
        TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &frame.images);
    stats.frames = stats.frames.saturating_add(1);
    stats.commands = stats.commands.saturating_add(render_frame.commands.len());
    stats.cells = stats.cells.saturating_add(frame.stats.cells);
    stats.chars = stats.chars.saturating_add(frame.stats.chars);
    stats.hash ^= frame
        .text
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
            (hash ^ u64::from(u32::from(*ch))).wrapping_mul(0x0000_0100_0000_01b3)
        });
    Ok(())
}

fn run_power_case(case: PowerCase, seconds: u32) -> Result<u64> {
    let mut engines = (0..case.modeled_instances())
        .map(|_| terminal_engine())
        .collect::<Result<Vec<_>>>()?;
    let mut planners = (0..engines.len())
        .map(|_| PaintPlanner::default())
        .collect::<Vec<_>>();
    let ticks = seconds.saturating_mul(case.target_hz()).max(1);
    let mut stats = PowerStats {
        instances: engines.len(),
        ..PowerStats::default()
    };

    for tick in 0..ticks {
        for (engine, planner) in engines.iter_mut().zip(planners.iter_mut()) {
            write_case_frame(engine, case, tick);
            render_once(engine, planner, &mut stats)?;
        }
    }

    stats.checksum()
}

fn bench_power_workloads(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    for case in CASES {
        c.bench_function(
            &format!("power_thermal_{}_1s_render_model", case.name()),
            |b| {
                b.iter_batched(
                    || case,
                    |case| {
                        black_box(run_power_case(case, 1).map_err(|error| failure = Some(error)))
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    failure.map_or(Ok(()), Err)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .sample_size(10)
        .noise_threshold(0.20)
        .configure_from_args();
    bench_power_workloads(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
