use anyhow::Result;
use bootty_terminal::geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface};
use bootty_terminal::{terminal_engine::TerminalEngine, terminal_frame::RenderFrame};
use bootty_ui::{
    paint_plan::PaintPlanner,
    terminal_render::TerminalRenderFrame,
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};

const BASE64_TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub type ScenarioBuilder = (&'static str, fn() -> Result<TerminalEngine>);

pub struct PreparedScenario {
    pub name: &'static str,
    pub frame: RenderFrame,
    pub surface: TerminalSurface,
}

#[allow(dead_code)]
pub struct PreparedRenderScenario {
    pub name: &'static str,
    pub frame: TerminalRenderFrame,
}

pub fn terminal_engine(cols: u16, rows: u16) -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })
}

pub fn surface_for(cols: u16, rows: u16) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::mul_add(f32::from(cols), 9.0, 20.0),
        f32::mul_add(f32::from(rows), 22.0, 20.0),
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::uniform(10.0),
    )
}

fn sample_engine() -> Result<TerminalEngine> {
    let mut engine = terminal_engine(120, 40)?;

    for row in 0_usize..40 {
        let line = format!(
            "\x1b[{};1Hrow {:03}  abcdefghijklmnopqrstuvwxyz  0123456789",
            row.saturating_add(1),
            row
        );
        engine.write_vt(line.as_bytes());
    }

    Ok(engine)
}

fn complex_shell_engine() -> Result<TerminalEngine> {
    let mut engine = terminal_engine(180, 80)?;

    for row in 0_usize..80 {
        let hue = row.saturating_mul(5) % 255;
        let alt = row.saturating_mul(11).saturating_add(40) % 255;
        let line = format!(
            "\x1b[{};1H\
             \x1b[1;38;2;125;207;255mrow {row:03}\x1b[0m \
             \x1b[48;5;238;38;5;{}mindexed-bg\x1b[0m \
             \x1b[3;4;38;2;{};{};210municode 🥟 界 café e\u{301} \x1b[0m\
             \x1b[38;5;214m█░▒▓ ┃╋╬╣╠╦╩\x1b[0m \
             \x1b[38;2;{};160;{}m λ ∑ → ←\x1b[0m \
             \x1b[38;5;{}m{}\x1b[0m",
            row.saturating_add(1),
            16_usize.saturating_add(row % 216),
            hue,
            255_usize.saturating_sub(hue),
            alt,
            255_usize.saturating_sub(alt),
            196_usize.saturating_add(row % 36),
            "0123456789abcdef".repeat(5)
        );
        engine.write_vt(line.as_bytes());
    }

    Ok(engine)
}

fn ai_agent_dashboard_engine() -> Result<TerminalEngine> {
    let mut engine = terminal_engine(220, 70)?;
    engine.write_vt(b"\x1b[?25l");
    write_agent_dashboard_frame(&mut engine, 37, 220, 70);
    Ok(engine)
}

fn tmux_image_truecolor_engine() -> Result<TerminalEngine> {
    let mut engine = terminal_engine(240, 90)?;
    engine.write_vt(b"\x1b[?25l");
    write_tmux_layout(&mut engine, 240, 90);
    write_kitty_image_grid(&mut engine);
    Ok(engine)
}

pub fn scenario_builders() -> [ScenarioBuilder; 4] {
    [
        ("simple_shell_120x40", sample_engine),
        ("complex_shell_180x80", complex_shell_engine),
        ("ai_agent_dashboard_220x70", ai_agent_dashboard_engine),
        ("tmux_images_truecolor_240x90", tmux_image_truecolor_engine),
    ]
}

pub fn prepared_scenarios() -> Result<Vec<PreparedScenario>> {
    scenario_builders()
        .into_iter()
        .map(|(name, builder)| {
            let mut engine = builder()?;
            let (cols, rows) = engine.grid_size();
            Ok(PreparedScenario {
                name,
                frame: engine.extract_frame()?.clone(),
                surface: surface_for(cols, rows),
            })
        })
        .collect()
}

#[allow(dead_code)]
pub fn prepared_render_scenarios() -> Result<Vec<PreparedRenderScenario>> {
    Ok(prepared_scenarios()?
        .into_iter()
        .map(|scenario| {
            let mut planner = PaintPlanner::default();
            let plan = planner
                .plan(scenario.surface, &scenario.frame, 16.0)
                .clone();
            let text_contract = TerminalTextContract::for_terminal_paint_plan(
                &plan,
                &TerminalTextConfig::default(),
            );
            PreparedRenderScenario {
                name: scenario.name,
                frame: TerminalRenderFrame::from_plan(&plan, &text_contract),
            }
        })
        .collect())
}

#[allow(dead_code)]
pub fn agent_render_frame(
    engine: &mut TerminalEngine,
    planner: &mut PaintPlanner,
    surface: TerminalSurface,
    tick: u32,
) -> Result<TerminalRenderFrame> {
    write_agent_dashboard_frame(engine, tick, 160, 60);
    let frame = engine.extract_frame()?;
    let plan = planner.plan(surface, frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    Ok(TerminalRenderFrame::from_plan(&plan, &text_contract))
}

#[allow(dead_code)]
pub fn mutate_single_row(engine: &mut TerminalEngine, tick: u32) {
    let row = tick
        .checked_rem(u32::from(engine.grid_size().1))
        .unwrap_or(0)
        .saturating_add(1);
    engine.write_vt(format!("\x1b[{row};1Htick-{tick:08x}").as_bytes());
}

pub fn write_agent_dashboard_frame(engine: &mut TerminalEngine, tick: u32, cols: u16, rows: u16) {
    let spinner = match tick % 8 {
        0 => "⠋",
        1 => "⠙",
        2 => "⠹",
        3 => "⠸",
        4 => "⠼",
        5 => "⠴",
        6 => "⠦",
        _ => "⠧",
    };
    let progress = tick % 101;
    let phase = match (tick / 7) % 4 {
        0 => "planning",
        1 => "running tools",
        2 => "patching",
        _ => "verifying",
    };
    let width = cols.saturating_sub(2);

    engine.write_vt(b"\x1b[H\x1b[0m");
    engine.write_vt(
        format!(
            "\x1b[48;2;17;24;39;38;2;192;202;245m {spinner} Bootty agent · {phase:<14} · {progress:>3}% · tokens 184,392 · jobs 7/9 {}\x1b[0m",
            " ".repeat(usize::from(width.saturating_sub(88))),
        )
        .as_bytes(),
    );

    for row in 1..rows {
        let row_u32 = u32::from(row);
        let tool = match row % 6 {
            0 => "shell",
            1 => "patch",
            2 => "cargo",
            3 => "sym",
            4 => "render",
            _ => "pty",
        };
        let status = match row % 5 {
            0 => "queued",
            1 => "running",
            2 => "streaming",
            3 => "cached",
            _ => "done",
        };
        let color = 80_u32.saturating_add(row_u32.saturating_mul(3) % 150);
        let heat = 30_u32.saturating_add(row_u32.saturating_mul(5) % 190);
        let gutter = if row % 9 == 0 {
            "\x1b[38;2;255;158;100m│ warn\x1b[0m"
        } else if row % 13 == 0 {
            "\x1b[38;2;255;90;90m│ err \x1b[0m"
        } else {
            "\x1b[38;2;125;207;255m│ info\x1b[0m"
        };
        let diff = if row % 4 == 0 {
            "\x1b[38;2;158;206;106m+ async drain budget respected\x1b[0m"
        } else if row % 4 == 1 {
            "\x1b[38;2;247;118;142m- redundant redraw removed\x1b[0m"
        } else {
            "\x1b[38;2;224;175;104m~ streaming stdout chunk → renderer batch\x1b[0m"
        };
        let glyphs = match row % 5 {
            0 => "🥟",
            1 => "",
            2 => "█▓▒░",
            3 => "╭─╮",
            _ => "λ∑→←",
        };
        engine.write_vt(
            format!(
                "\x1b[{};1H{gutter} \
                 \x1b[38;2;{};{};230m{tool:<6}\x1b[0m \
                 \x1b[48;5;236;38;5;{}m{status:<9}\x1b[0m \
                 \x1b[38;2;{};180;{}magent-{row:03}\x1b[0m \
                 {diff} \
                 \x1b[38;5;{}m{glyphs}\x1b[0m \
                 {}",
                row.saturating_add(1),
                color,
                255_u32.saturating_sub(color),
                16_u16.saturating_add(row % 216),
                heat,
                255_u32.saturating_sub(heat),
                160_u16.saturating_add(row % 60),
                "trace=".repeat(8),
            )
            .as_bytes(),
        );
    }
}

fn write_tmux_layout(engine: &mut TerminalEngine, cols: u16, rows: u16) {
    let bottom = rows.saturating_sub(2);
    let right = cols.saturating_sub(1);
    let split_x = cols.saturating_mul(3) / 5;
    let split_y = rows.saturating_mul(2) / 5;

    engine.write_vt(b"\x1b[H\x1b[0m");
    engine.write_vt(
        format!(
            "\x1b[38;2;125;207;255m╭{}╮\x1b[0m",
            "─".repeat(usize::from(right.saturating_sub(1)))
        )
        .as_bytes(),
    );
    for row in 2..bottom {
        engine.write_vt(format!("\x1b[{row};1H\x1b[38;2;125;207;255m│\x1b[0m").as_bytes());
        engine.write_vt(format!("\x1b[{row};{right}H\x1b[38;2;125;207;255m│\x1b[0m").as_bytes());
        engine.write_vt(format!("\x1b[{row};{split_x}H\x1b[38;2;86;95;137m┃\x1b[0m").as_bytes());
    }
    engine.write_vt(
        format!(
            "\x1b[{bottom};1H\x1b[38;2;125;207;255m╰{}╯\x1b[0m",
            "─".repeat(usize::from(right.saturating_sub(1)))
        )
        .as_bytes(),
    );
    engine.write_vt(
        format!(
            "\x1b[{split_y};2H\x1b[38;2;86;95;137m{}\x1b[0m",
            "━".repeat(usize::from(right.saturating_sub(3)))
        )
        .as_bytes(),
    );
    engine.write_vt(
        format!(
            "\x1b[{};1H\x1b[48;2;31;41;59;38;2;192;202;245m[bootty] 0:zsh* 1:agents 2:logs 3:images  cpu 73% mem 12.4G{}\x1b[0m",
            rows,
            " ".repeat(usize::from(cols.saturating_sub(67)))
        )
        .as_bytes(),
    );

    for row in 3..split_y {
        engine.write_vt(
            format!(
                "\x1b[{row};3H\x1b[38;2;158;206;106magent\x1b[0m \
                 \x1b[38;2;224;175;104mtool_call\x1b[0m \
                 id=call_{row:03} stdout={} 🥟",
                "stream ".repeat(9)
            )
            .as_bytes(),
        );
    }
    for row in split_y.saturating_add(1)..bottom {
        let pct = row.saturating_mul(100).checked_div(bottom).unwrap_or(0);
        engine.write_vt(
            format!(
                "\x1b[{row};3H\x1b[38;2;247;118;142mhtop\x1b[0m pid={:05} \
                 \x1b[48;2;40;42;54;38;2;125;207;255m{:>3}% {}\x1b[0m \
                 \x1b[38;5;214m╭─╮ ┃╋ █▓▒░ \x1b[0m",
                10_000_u16.saturating_add(row),
                pct,
                "▰".repeat(usize::from(pct / 5)),
            )
            .as_bytes(),
        );
    }
}

fn write_kitty_image_grid(engine: &mut TerminalEngine) {
    for image_index in 0_usize..8 {
        let index = u32::try_from(image_index).unwrap_or(u32::MAX);
        let id = 400_u32.saturating_add(index);
        let x = 150_u32.saturating_add((index % 2).saturating_mul(35));
        let y = 5_u32.saturating_add((index / 2).saturating_mul(18));
        engine.write_vt(
            rgb_image_command(&RgbImageCommand {
                image_id: id,
                pixel_width: 28,
                pixel_height: 12,
                cols: 30,
                rows: 12,
                x,
                y,
                seed: index,
            })
            .as_bytes(),
        );
    }
}

#[derive(Clone, Copy)]
struct RgbImageCommand {
    image_id: u32,
    pixel_width: u32,
    pixel_height: u32,
    cols: u32,
    rows: u32,
    x: u32,
    y: u32,
    seed: u32,
}

fn rgb_image_command(command: &RgbImageCommand) -> String {
    let RgbImageCommand {
        image_id,
        pixel_width,
        pixel_height,
        cols,
        rows,
        x,
        y,
        seed,
    } = *command;
    let capacity = pixel_width.saturating_mul(pixel_height).saturating_mul(3);
    let mut bytes = Vec::with_capacity(usize::try_from(capacity).unwrap_or(usize::MAX));
    for py in 0..pixel_height {
        for px in 0..pixel_width {
            let red = px.wrapping_mul(7).wrapping_add(seed.wrapping_mul(23)) % 255;
            let green = py.wrapping_mul(11).wrapping_add(seed.wrapping_mul(31)) % 255;
            let blue = px
                .wrapping_add(py)
                .wrapping_mul(5)
                .wrapping_add(seed.wrapping_mul(17))
                % 255;
            bytes.push(u8::try_from(red).unwrap_or(0));
            bytes.push(u8::try_from(green).unwrap_or(0));
            bytes.push(u8::try_from(blue).unwrap_or(0));
        }
    }
    format!(
        "\x1b_Ga=T,t=d,f=24,i={image_id},p=1,s={pixel_width},v={pixel_height},c={cols},r={rows},x={x},y={y},q=1;{}\x1b\\",
        base64_encode_bytes(&bytes)
    )
}

fn base64_encode_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));

    for chunk in bytes.chunks(3) {
        let b0 = chunk.first().copied().unwrap_or(0);
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);

        out.push(base64_char(b0 >> 2));
        out.push(base64_char(((b0 & 0b0000_0011) << 4) | (b1 >> 4)));
        if chunk.len() > 1 {
            out.push(base64_char(((b1 & 0b0000_1111) << 2) | (b2 >> 6)));
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(base64_char(b2 & 0b0011_1111));
        } else {
            out.push('=');
        }
    }

    out
}

fn base64_char(index: u8) -> char {
    BASE64_TABLE
        .get(usize::from(index))
        .copied()
        .map_or('?', char::from)
}
