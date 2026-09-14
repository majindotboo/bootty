use std::hint::black_box;

use anyhow::{Result, anyhow};

mod paint_plan_fixtures;

use bootty_config::config::{BoottyConfig, MultiplexerBackendConfig};
use bootty_terminal::geometry::TerminalSurface;
use bootty_terminal::{
    terminal_engine::TerminalEngine,
    terminal_input_model::{KeyInput, KeyMods, TerminalKey},
};
use bootty_ui::{
    app_actions::{AppKeyBindings, KeybindAction, MuxKeyAction},
    commands::{CommandExecutor, CommandRegistry, CoreCommandExecutor},
};
use bootty_ui::{
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};
use criterion::Criterion;
use paint_plan_fixtures::{surface_for, terminal_engine};

const PANE_COUNTS: [usize; 4] = [1, 4, 16, 64];
const TAB_COUNTS: [usize; 3] = [1, 8, 32];
const MULTI_WINDOW_COUNTS: [usize; 3] = [1, 4, 16];
const PANE_COLS: u16 = 240;
const PANE_ROWS: u16 = 90;
const WINDOW_COLS: u16 = 120;
const WINDOW_ROWS: u16 = 40;

#[derive(Clone, Copy)]
struct PaneBounds {
    left: u16,
    top: u16,
    width: u16,
    height: u16,
}

#[derive(Clone, Copy)]
enum MuxEquivalent {
    Native,
    Tmux,
}

impl MuxEquivalent {
    const fn label(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::Tmux => "tmux",
        }
    }

    const fn status_prefix(self) -> &'static str {
        match self {
            Self::Native => "bootty",
            Self::Tmux => "tmux",
        }
    }
}

const MUX_EQUIVALENTS: [MuxEquivalent; 2] = [MuxEquivalent::Native, MuxEquivalent::Tmux];

const fn pane_grid_for(count: usize) -> Option<(usize, usize)> {
    match count {
        1 => Some((1, 1)),
        4 => Some((2, 2)),
        16 => Some((4, 4)),
        64 => Some((8, 8)),
        _ => None,
    }
}

fn pane_bounds(index: usize, count: usize, cols: u16, rows: u16) -> Result<PaneBounds> {
    let (grid_cols, grid_rows) =
        pane_grid_for(count).ok_or_else(|| anyhow!("unsupported pane count {count}"))?;
    let cell_width = usize::from(cols).checked_div(grid_cols).unwrap_or(0);
    let usable_rows = usize::from(rows.saturating_sub(1));
    let cell_height = usable_rows.checked_div(grid_rows).unwrap_or(0);
    let grid_col = index.checked_rem(grid_cols).unwrap_or(0);
    let grid_row = index.checked_div(grid_cols).unwrap_or(0);
    let left = grid_col.saturating_mul(cell_width).saturating_add(1);
    let top = grid_row.saturating_mul(cell_height).saturating_add(1);
    let right = if grid_col.saturating_add(1) == grid_cols {
        usize::from(cols)
    } else {
        grid_col.saturating_add(1).saturating_mul(cell_width)
    };
    let bottom = if grid_row.saturating_add(1) == grid_rows {
        usable_rows
    } else {
        grid_row.saturating_add(1).saturating_mul(cell_height)
    };
    Ok(PaneBounds {
        left: u16::try_from(left)?,
        top: u16::try_from(top)?,
        width: u16::try_from(right.saturating_sub(left).saturating_add(1))?,
        height: u16::try_from(bottom.saturating_sub(top).saturating_add(1))?,
    })
}

fn write_at(engine: &mut TerminalEngine, row: u16, col: u16, text: &str) {
    engine.write_vt(format!("\x1b[{row};{col}H{text}").as_bytes());
}

fn clipped_ascii(value: &str, width: u16) -> String {
    value.chars().take(usize::from(width)).collect()
}

fn write_pane_frame(engine: &mut TerminalEngine, bounds: PaneBounds, pane: usize, active: bool) {
    if bounds.width < 4 || bounds.height < 3 {
        return;
    }
    let inner_width = bounds.width.saturating_sub(2);
    let right = bounds.left.saturating_add(bounds.width).saturating_sub(1);
    let bottom = bounds.top.saturating_add(bounds.height).saturating_sub(1);
    let color = if active { "38;5;81" } else { "38;5;240" };
    let header = format!("pane-{pane:02} {}", if active { "ACTIVE" } else { "idle" });

    write_at(
        engine,
        bounds.top,
        bounds.left,
        &format!(
            "\x1b[{color}m+{}+\x1b[0m",
            "-".repeat(usize::from(inner_width))
        ),
    );
    for row in bounds.top.saturating_add(1)..bottom {
        write_at(engine, row, bounds.left, &format!("\x1b[{color}m|\x1b[0m"));
        write_at(engine, row, right, &format!("\x1b[{color}m|\x1b[0m"));
    }
    write_at(
        engine,
        bottom,
        bounds.left,
        &format!(
            "\x1b[{color}m+{}+\x1b[0m",
            "-".repeat(usize::from(inner_width))
        ),
    );
    write_at(
        engine,
        bounds.top,
        bounds.left.saturating_add(2),
        &format!(
            "\x1b[1;{color}m{}\x1b[0m",
            clipped_ascii(&header, inner_width)
        ),
    );

    let content_rows = bounds.height.saturating_sub(2).min(6);
    for line in 0..content_rows {
        let text = format!(
            "job={pane:02}.{line:02} cpu={:02}% rss={:04}M stream {}",
            pane.saturating_mul(7)
                .saturating_add(usize::from(line).saturating_mul(13))
                % 100,
            128_usize
                .saturating_add(pane.saturating_mul(11))
                .saturating_add(usize::from(line)),
            if active { "tailing" } else { "parked" }
        );
        write_at(
            engine,
            bounds.top.saturating_add(1).saturating_add(line),
            bounds.left.saturating_add(2),
            &format!(
                "\x1b[38;5;{}m{}\x1b[0m",
                70_usize.saturating_add(pane % 120),
                clipped_ascii(&text, inner_width)
            ),
        );
    }
}

fn write_pane_grid(
    engine: &mut TerminalEngine,
    pane_count: usize,
    active_pane: usize,
    tick: u32,
) -> Result<()> {
    engine.write_vt(b"\x1b[?25l\x1b[H\x1b[0m\x1b[2J");
    for pane in 0..pane_count {
        write_pane_frame(
            engine,
            pane_bounds(pane, pane_count, PANE_COLS, PANE_ROWS)?,
            pane,
            pane == active_pane,
        );
    }
    write_at(
        engine,
        PANE_ROWS,
        1,
        &format!(
            "\x1b[48;5;236;38;5;252m bootty mux-equivalent panes={} active={} tick={} {}\x1b[0m",
            pane_count,
            active_pane,
            tick,
            " ".repeat(usize::from(PANE_COLS.saturating_sub(58)))
        ),
    );
    Ok(())
}

fn write_active_pane_update(
    engine: &mut TerminalEngine,
    pane_count: usize,
    active_pane: usize,
    tick: u32,
) -> Result<()> {
    let bounds = pane_bounds(active_pane, pane_count, PANE_COLS, PANE_ROWS)?;
    let tick_u16 = u16::try_from(tick & u32::from(u16::MAX)).unwrap_or(u16::MAX);
    let row = bounds.top.saturating_add(1).saturating_add(
        tick_u16
            .checked_rem(bounds.height.saturating_sub(2).max(1))
            .unwrap_or(0),
    );
    let text =
        format!("input tick={tick:08x} pane={active_pane:02} latency-probe keypress echo burst");
    write_at(
        engine,
        row,
        bounds.left.saturating_add(2),
        &format!(
            "\x1b[48;5;24;38;5;231m{}\x1b[0m",
            clipped_ascii(&text, bounds.width.saturating_sub(4))
        ),
    );
    Ok(())
}

fn write_mux_equivalent_chrome(
    engine: &mut TerminalEngine,
    mode: MuxEquivalent,
    pane_count: usize,
    tab_count: usize,
    active_tab: usize,
    tick: u32,
) {
    let prefix = mode.status_prefix();
    match mode {
        MuxEquivalent::Native => {
            write_at(
                engine,
                1,
                1,
                &format!(
                    "\x1b[48;5;24;38;5;231m {prefix} tabs={tab_count} active={active_tab:02} panes={pane_count} tick={tick:08x} {}\x1b[0m",
                    " ".repeat(usize::from(PANE_COLS.saturating_sub(64)))
                ),
            );
        }
        MuxEquivalent::Tmux => {
            write_at(
                engine,
                1,
                1,
                &format!(
                    "\x1b[48;5;22;38;5;231m[{}] {}\x1b[0m",
                    (0..tab_count)
                        .map(|tab| if tab == active_tab {
                            format!("#{tab}:active*")
                        } else {
                            format!("#{tab}:idle")
                        })
                        .collect::<Vec<_>>()
                        .join(" "),
                    " ".repeat(usize::from(
                        PANE_COLS
                            .saturating_sub(
                                u16::try_from(tab_count)
                                    .unwrap_or(u16::MAX)
                                    .saturating_mul(10)
                            )
                            .max(1),
                    ))
                ),
            );
            write_at(
                engine,
                PANE_ROWS,
                1,
                &format!(
                    "\x1b[48;5;236;38;5;252m {prefix}: panes={pane_count} ctrl-b passthrough tick={tick:08x} {}\x1b[0m",
                    " ".repeat(usize::from(PANE_COLS.saturating_sub(64)))
                ),
            );
        }
    }
}

fn mux_equivalent_engine(
    mode: MuxEquivalent,
    pane_count: usize,
    tab_count: usize,
) -> Result<TerminalEngine> {
    let mut engine = pane_engine(pane_count)?;
    write_mux_equivalent_chrome(&mut engine, mode, pane_count, tab_count, 0, 0);
    Ok(engine)
}

fn write_all_panes_tailing(
    engine: &mut TerminalEngine,
    mode: MuxEquivalent,
    pane_count: usize,
    tab_count: usize,
    tick: u32,
) -> Result<()> {
    let active_tab = usize::try_from(tick)
        .unwrap_or(usize::MAX)
        .checked_rem(tab_count)
        .unwrap_or(0);
    for pane in 0..pane_count {
        let bounds = pane_bounds(pane, pane_count, PANE_COLS, PANE_ROWS)?;
        let content_rows = bounds.height.saturating_sub(2).max(1);
        let tick_u16 = u16::try_from(tick & u32::from(u16::MAX)).unwrap_or(u16::MAX);
        let pane_u16 = u16::try_from(pane & usize::from(u16::MAX)).unwrap_or(u16::MAX);
        let row = bounds.top.saturating_add(1).saturating_add(
            tick_u16
                .saturating_add(pane_u16)
                .checked_rem(content_rows)
                .unwrap_or(0),
        );
        let text = format!(
            "{} pane={pane:02} tab={active_tab:02} line={} cpu={:02}% tail {}",
            mode.label(),
            tick.wrapping_add(u32::try_from(pane).unwrap_or(u32::MAX)),
            usize::try_from(tick)
                .unwrap_or(usize::MAX)
                .saturating_mul(3)
                .saturating_add(pane.saturating_mul(11))
                % 100,
            "log ".repeat(6)
        );
        write_at(
            engine,
            row,
            bounds.left.saturating_add(2),
            &format!(
                "\x1b[38;5;{}m{}\x1b[0m",
                100_usize.saturating_add(pane % 80),
                clipped_ascii(&text, bounds.width.saturating_sub(4))
            ),
        );
    }
    write_mux_equivalent_chrome(engine, mode, pane_count, tab_count, active_tab, tick);
    Ok(())
}

fn pane_engine(pane_count: usize) -> Result<TerminalEngine> {
    let mut engine = terminal_engine(PANE_COLS, PANE_ROWS)?;
    write_pane_grid(&mut engine, pane_count, 0, 0)?;
    Ok(engine)
}

fn extract_plan_render(
    engine: &mut TerminalEngine,
    planner: &mut PaintPlanner,
    surface: TerminalSurface,
) -> Result<usize> {
    let frame = engine.extract_frame()?;
    let plan = planner.plan(surface, frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    Ok(TerminalRenderFrame::from_plan(&plan, &text_contract)
        .commands
        .len())
}

fn write_window_frame(engine: &mut TerminalEngine, window: usize, tick: u32) {
    engine.write_vt(b"\x1b[H\x1b[0m");
    engine.write_vt(
        format!(
            "\x1b[48;5;24;38;5;231m window {window:02} active frame {tick:08x} {}\x1b[0m",
            " ".repeat(76)
        )
        .as_bytes(),
    );
    for row in 2..WINDOW_ROWS {
        engine.write_vt(
            format!(
                "\x1b[{row};1H\x1b[38;5;{}mwin={window:02} row={row:02} log={}\x1b[0m",
                80_usize.saturating_add((usize::from(row).saturating_add(window)) % 120),
                "event ".repeat(10)
            )
            .as_bytes(),
        );
    }
}

fn window_engines(count: usize) -> Result<Vec<TerminalEngine>> {
    (0..count)
        .map(|window| {
            let mut engine = terminal_engine(WINDOW_COLS, WINDOW_ROWS)?;
            write_window_frame(&mut engine, window, 0);
            Ok(engine)
        })
        .collect()
}

fn tab_key_inputs() -> Vec<KeyInput> {
    vec![
        KeyInput {
            key: TerminalKey::Tab,
            mods: KeyMods {
                ctrl: true,
                ..Default::default()
            },
            repeat: false,
            utf8: None,
            unshifted: None,
        },
        KeyInput {
            key: TerminalKey::Digit1,
            mods: KeyMods {
                command: true,
                ..Default::default()
            },
            repeat: false,
            utf8: None,
            unshifted: None,
        },
    ]
}

fn bench_pane_grid_pipeline(c: &mut Criterion) -> Result<()> {
    for pane_count in PANE_COUNTS {
        let mut engine = pane_engine(pane_count)?;
        let mut planner = PaintPlanner::default();
        let surface = surface_for(PANE_COLS, PANE_ROWS);
        let mut tick = 0_u32;
        let mut failure = None;
        c.bench_function(
            &format!("pane_grid_extract_plan_render_{pane_count}_panes_240x90"),
            |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    let result = (|| -> Result<_> {
                        let active_pane = usize::try_from(tick)
                            .unwrap_or(usize::MAX)
                            .checked_rem(pane_count)
                            .unwrap_or(0);
                        write_active_pane_update(&mut engine, pane_count, active_pane, tick)?;
                        extract_plan_render(&mut engine, &mut planner, surface)
                    })();
                    black_box(result.map_err(|error| failure = Some(error)))
                });
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(())
}

fn bench_pane_active_and_inactive_paths(c: &mut Criterion) -> Result<()> {
    for pane_count in PANE_COUNTS {
        let mut engine = pane_engine(pane_count)?;
        let mut tick = 0_u32;
        let mut failure = None;
        c.bench_function(
            &format!("pane_active_update_write_{pane_count}_panes_240x90"),
            |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    let result = (|| -> Result<_> {
                        let active_pane = usize::try_from(tick)
                            .unwrap_or(usize::MAX)
                            .checked_rem(pane_count)
                            .unwrap_or(0);
                        write_active_pane_update(&mut engine, pane_count, active_pane, tick)?;
                        Ok(engine.grid_size())
                    })();
                    black_box(result.map_err(|error| failure = Some(error)))
                });
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }

        black_box(engine.extract_frame()?.stats.cells);
        let mut inactive_failure = None;
        c.bench_function(
            &format!("pane_inactive_clean_extract_{pane_count}_panes_240x90"),
            |b| {
                b.iter(|| {
                    let result = engine.extract_frame().map(|frame| frame.stats.cells);
                    black_box(result.map_err(|error| inactive_failure = Some(error)))
                });
            },
        );
        if let Some(error) = inactive_failure {
            return Err(error);
        }
    }
    Ok(())
}

fn bench_tab_keybind_lookup(c: &mut Criterion) -> Result<()> {
    let keybinds = BoottyConfig::default()
        .input
        .keybinds_for_backend(MultiplexerBackendConfig::Native);
    let inputs = tab_key_inputs();

    let mut bindings = AppKeyBindings::from_keybinds(&keybinds)?;
    c.bench_function("tab_switch_keybinding_lookup_native", |b| {
        b.iter(|| {
            let mut hits = 0_u8;
            for input in &inputs {
                let executor =
                    bindings
                        .invocation_for_input(black_box(*input))
                        .and_then(|invocation| {
                            CommandRegistry::core()
                                .resolve(invocation)
                                .ok()
                                .map(|resolved| resolved.executor)
                        });
                if matches!(
                    executor,
                    Some(CommandExecutor::Core(CoreCommandExecutor::Keybind(
                        KeybindAction::Mux(MuxKeyAction::NextTab | MuxKeyAction::SelectTab(_))
                    )))
                ) {
                    hits = hits.saturating_add(1);
                }
            }
            black_box(hits)
        });
    });
    Ok(())
}

fn bench_multi_window_frame_paths(c: &mut Criterion) -> Result<()> {
    for window_count in MULTI_WINDOW_COUNTS {
        let mut engines = window_engines(window_count)?;
        let mut planners = (0..window_count)
            .map(|_| PaintPlanner::default())
            .collect::<Vec<_>>();
        let surface = surface_for(WINDOW_COLS, WINDOW_ROWS);
        let mut tick = 0_u32;
        let mut failure = None;
        c.bench_function(
            &format!("multi_window_extract_plan_render_{window_count}_windows_120x40"),
            |b| {
                b.iter(|| {
                    tick = tick.wrapping_add(1);
                    let mut commands = 0_usize;
                    for (index, (engine, planner)) in
                        engines.iter_mut().zip(planners.iter_mut()).enumerate()
                    {
                        write_window_frame(engine, index, tick);
                        match extract_plan_render(engine, planner, surface) {
                            Ok(count) => commands = commands.saturating_add(count),
                            Err(error) => failure = Some(error),
                        }
                    }
                    black_box(commands)
                });
            },
        );
        if let Some(error) = failure {
            return Err(error);
        }
    }
    Ok(())
}

fn bench_mux_equivalent_pane_modes(c: &mut Criterion) -> Result<()> {
    for mode in MUX_EQUIVALENTS {
        for pane_count in PANE_COUNTS {
            let mut engine = mux_equivalent_engine(mode, pane_count, 8)?;
            let mut planner = PaintPlanner::default();
            let surface = surface_for(PANE_COLS, PANE_ROWS);
            let mut tick = 0_u32;
            let mut failure = None;
            c.bench_function(
                &format!(
                    "mux_equivalent_{}_mixed_active_{pane_count}_panes",
                    mode.label()
                ),
                |b| {
                    b.iter(|| {
                        tick = tick.wrapping_add(1);
                        let result = (|| -> Result<_> {
                            let tick_index = usize::try_from(tick).unwrap_or(usize::MAX);
                            let active_pane = tick_index.checked_rem(pane_count).unwrap_or(0);
                            write_active_pane_update(&mut engine, pane_count, active_pane, tick)?;
                            write_mux_equivalent_chrome(
                                &mut engine,
                                mode,
                                pane_count,
                                8,
                                tick_index.checked_rem(8).unwrap_or(0),
                                tick,
                            );
                            extract_plan_render(&mut engine, &mut planner, surface)
                        })();
                        black_box(result.map_err(|error| failure = Some(error)))
                    });
                },
            );
            if let Some(error) = failure {
                return Err(error);
            }

            let mut engine = mux_equivalent_engine(mode, pane_count, 8)?;
            let mut planner = PaintPlanner::default();
            let surface = surface_for(PANE_COLS, PANE_ROWS);
            let mut tick = 0_u32;
            let mut failure = None;
            c.bench_function(
                &format!(
                    "mux_equivalent_{}_all_tailing_{pane_count}_panes",
                    mode.label()
                ),
                |b| {
                    b.iter(|| {
                        tick = tick.wrapping_add(1);
                        let result = (|| -> Result<_> {
                            write_all_panes_tailing(&mut engine, mode, pane_count, 8, tick)?;
                            extract_plan_render(&mut engine, &mut planner, surface)
                        })();
                        black_box(result.map_err(|error| failure = Some(error)))
                    });
                },
            );
            if let Some(error) = failure {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn bench_mux_equivalent_tab_modes(c: &mut Criterion) -> Result<()> {
    for mode in MUX_EQUIVALENTS {
        for tab_count in TAB_COUNTS {
            let mut engine = mux_equivalent_engine(mode, 4, tab_count)?;
            let mut planner = PaintPlanner::default();
            let surface = surface_for(PANE_COLS, PANE_ROWS);
            let mut tick = 0_u32;
            let mut failure = None;
            c.bench_function(
                &format!(
                    "mux_equivalent_{}_tab_switch_{tab_count}_tabs",
                    mode.label()
                ),
                |b| {
                    b.iter(|| {
                        tick = tick.wrapping_add(1);
                        let result = {
                            let tick_index = usize::try_from(tick).unwrap_or(usize::MAX);
                            write_mux_equivalent_chrome(
                                &mut engine,
                                mode,
                                4,
                                tab_count,
                                tick_index.checked_rem(tab_count).unwrap_or(0),
                                tick,
                            );
                            extract_plan_render(&mut engine, &mut planner, surface)
                        };
                        black_box(result.map_err(|error| failure = Some(error)))
                    });
                },
            );
            if let Some(error) = failure {
                return Err(error);
            }
        }
    }
    Ok(())
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .noise_threshold(0.15)
        .configure_from_args();
    bench_pane_grid_pipeline(&mut criterion)?;
    bench_pane_active_and_inactive_paths(&mut criterion)?;
    bench_tab_keybind_lookup(&mut criterion)?;
    bench_multi_window_frame_paths(&mut criterion)?;
    bench_mux_equivalent_pane_modes(&mut criterion)?;
    bench_mux_equivalent_tab_modes(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
