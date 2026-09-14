use std::hint::black_box;

use anyhow::{Result, anyhow};
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
enum MuxStack {
    TerminalAlone,
    NativeMux,
    Tmux,
    Screen,
    TmuxOverSsh,
    NestedSshTmux,
}

impl MuxStack {
    const fn label(self) -> &'static str {
        match self {
            Self::TerminalAlone => "terminal_alone",
            Self::NativeMux => "native_mux",
            Self::Tmux => "tmux",
            Self::Screen => "screen",
            Self::TmuxOverSsh => "tmux_over_ssh",
            Self::NestedSshTmux => "nested_ssh_tmux",
        }
    }

    const fn term(self) -> &'static str {
        match self {
            Self::TerminalAlone | Self::NativeMux => "xterm-ghostty",
            Self::Tmux | Self::TmuxOverSsh | Self::NestedSshTmux => "tmux-256color",
            Self::Screen => "screen-256color",
        }
    }

    const fn supports_kitty_passthrough(self) -> bool {
        matches!(
            self,
            Self::TerminalAlone
                | Self::NativeMux
                | Self::Tmux
                | Self::TmuxOverSsh
                | Self::NestedSshTmux
        )
    }

    const fn wraps_kitty_passthrough(self) -> bool {
        matches!(self, Self::Tmux | Self::TmuxOverSsh | Self::NestedSshTmux)
    }

    const fn network_prefix(self) -> &'static [u8] {
        match self {
            Self::TmuxOverSsh => b"ssh hop ready\r\n",
            Self::NestedSshTmux => b"ssh bastion ready\r\nssh inner ready\r\n",
            _ => b"",
        }
    }
}

const STACKS: [MuxStack; 6] = [
    MuxStack::TerminalAlone,
    MuxStack::NativeMux,
    MuxStack::Tmux,
    MuxStack::Screen,
    MuxStack::TmuxOverSsh,
    MuxStack::NestedSshTmux,
];

#[derive(Default)]
struct MuxStats {
    bytes: usize,
    cells: usize,
    chars: usize,
    commands: usize,
    images: usize,
    side_effects: usize,
    feature_breakage: usize,
    hash: u64,
}

impl MuxStats {
    fn checksum(&self) -> Result<u64> {
        Ok(self.hash
            ^ u64::try_from(self.bytes)?
            ^ u64::try_from(self.cells)?.rotate_left(7)
            ^ u64::try_from(self.chars)?.rotate_left(13)
            ^ u64::try_from(self.commands)?.rotate_left(19)
            ^ u64::try_from(self.images)?.rotate_left(29)
            ^ u64::try_from(self.side_effects)?.rotate_left(37)
            ^ u64::try_from(self.feature_breakage)?.rotate_left(47))
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

fn hash_bytes(hash: u64, bytes: &[u8]) -> u64 {
    bytes.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn base64_encode_bytes(bytes: &[u8]) -> Result<String> {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3).saturating_mul(4));
    for chunk in bytes.chunks(3) {
        let b0 = *chunk
            .first()
            .ok_or_else(|| anyhow!("base64 chunk is empty"))?;
        let b1 = *chunk.get(1).unwrap_or(&0);
        let b2 = *chunk.get(2).unwrap_or(&0);
        out.push(char::from(
            *TABLE
                .get(usize::from(b0 >> 2))
                .ok_or_else(|| anyhow!("base64 index out of range"))?,
        ));
        out.push(char::from(
            *TABLE
                .get(usize::from(((b0 & 0b0000_0011) << 4) | (b1 >> 4)))
                .ok_or_else(|| anyhow!("base64 index out of range"))?,
        ));
        out.push(if chunk.len() > 1 {
            char::from(
                *TABLE
                    .get(usize::from(((b1 & 0b0000_1111) << 2) | (b2 >> 6)))
                    .ok_or_else(|| anyhow!("base64 index out of range"))?,
            )
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(
                *TABLE
                    .get(usize::from(b2 & 0b0011_1111))
                    .ok_or_else(|| anyhow!("base64 index out of range"))?,
            )
        } else {
            '='
        });
    }
    Ok(out)
}

fn raw_rgb_bytes(width: u32, height: u32, seed: u8) -> Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(usize::try_from(
        width.saturating_mul(height).saturating_mul(3),
    )?);
    for y in 0..height {
        for x in 0..width {
            bytes.push(u8::try_from(
                x.saturating_mul(7)
                    .saturating_add(u32::from(seed))
                    .rem_euclid(255),
            )?);
            bytes.push(u8::try_from(
                y.saturating_mul(11)
                    .saturating_add(u32::from(seed).saturating_mul(3))
                    .rem_euclid(255),
            )?);
            bytes.push(u8::try_from(
                x.saturating_add(y)
                    .saturating_mul(5)
                    .saturating_add(u32::from(seed).saturating_mul(9))
                    .rem_euclid(255),
            )?);
        }
    }
    Ok(bytes)
}

fn kitty_image_command(image_id: u32) -> Result<Vec<u8>> {
    Ok(format!(
        "\x1b_Ga=T,t=d,f=24,i={image_id},p={image_id},s=4,v=4,q=1;{}\x1b\\",
        base64_encode_bytes(&raw_rgb_bytes(4, 4, u8::try_from(image_id)?)?)?
    )
    .into_bytes())
}

fn tmux_wrap(payload: &[u8]) -> Vec<u8> {
    let mut out = b"\x1bPtmux;".to_vec();
    for byte in payload {
        if *byte == 0x1b {
            out.push(0x1b);
        }
        out.push(*byte);
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

fn maybe_passthrough(stack: MuxStack, payload: Vec<u8>) -> Vec<u8> {
    if stack.wraps_kitty_passthrough() {
        tmux_wrap(&payload)
    } else if stack.supports_kitty_passthrough() {
        payload
    } else {
        b"graphics passthrough unsupported; unicode fallback \xe2\x96\x80\xe2\x96\x84\xe2\x96\x88\r\n".to_vec()
    }
}

fn feature_payload(stack: MuxStack) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(stack.network_prefix());
    bytes
        .extend_from_slice(format!("TERM={} stack={}\r\n", stack.term(), stack.label()).as_bytes());
    bytes.extend_from_slice(b"\x1b[?2026h");
    bytes.extend_from_slice(b"\x1b[?1004h\x1b[?2004h\x1b[?1000h\x1b[?1006h");
    bytes.extend_from_slice(b"\x1b[38;2;116;199;236mtruecolor\x1b[0m ");
    bytes.extend_from_slice(
        b"\x1b]8;id=mux;https://example.invalid/mux\x1b\\osc8-link\x1b]8;;\x1b\\ ",
    );
    bytes.extend_from_slice(b"\x1b]52;c;Ym9vdHR5LW11eA==\x1b\\");
    bytes.extend_from_slice(&maybe_passthrough(stack, kitty_image_command(77)?));
    bytes.extend_from_slice(b"\x1b[?1006l\x1b[?1000l\x1b[?2004l\x1b[?1004l\x1b[?2026l\r\n");
    Ok(bytes)
}

fn throughput_payload(stack: MuxStack, lines: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(lines.saturating_mul(96));
    bytes.extend_from_slice(stack.network_prefix());
    for line in 0..lines {
        if line.rem_euclid(48) == 0 {
            bytes.extend_from_slice(
                format!(
                    "\x1b[48;5;236;38;5;252m{} status line {line:05}\x1b[0m\r\n",
                    stack.label()
                )
                .as_bytes(),
            );
        }
        bytes.extend_from_slice(
            format!(
                "\x1b[38;5;{}m{} line {line:05}: payload truecolor mouse paste osc8 sync logs {}\x1b[0m\r\n",
                70usize.saturating_add(line.rem_euclid(120)),
                stack.label(),
                "data ".repeat(5)
            )
            .as_bytes(),
        );
    }
    bytes
}

fn latency_payload(stack: MuxStack, keys: usize) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(keys.saturating_mul(48));
    bytes.extend_from_slice(stack.network_prefix());
    for key in 0..keys {
        bytes.extend_from_slice(
            format!(
                "\x1b[32m{} key-{key:03}\x1b[0m echo latency probe\r\n",
                stack.label()
            )
            .as_bytes(),
        );
    }
    bytes
}

fn extract_plan_render(engine: &mut TerminalEngine) -> Result<(usize, usize, usize, usize)> {
    let frame = engine.extract_frame()?;
    let cells = frame.cells.len();
    let chars = frame.text.len();
    let images = frame
        .images
        .placements
        .len()
        .saturating_add(frame.images.virtual_placements.len());
    let mut planner = PaintPlanner::default();
    let plan = planner.plan(surface(), frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    let commands = TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &frame.images)
        .commands
        .len();
    Ok((cells, chars, images, commands))
}

fn run_payload(mut engine: TerminalEngine, stack: MuxStack, payload: &[u8]) -> Result<u64> {
    engine.write_vt(payload);
    let side_effects = engine.drain_side_effects().len();
    let (cells, chars, images, commands) = extract_plan_render(&mut engine)?;
    let mut stats = MuxStats {
        bytes: payload.len(),
        cells,
        chars,
        commands,
        images,
        side_effects,
        feature_breakage: 0,
        hash: hash_bytes(0xcbf2_9ce4_8422_2325, stack.label().as_bytes()),
    };
    if stack.supports_kitty_passthrough() && stats.images == 0 {
        stats.feature_breakage = stats.feature_breakage.saturating_add(1);
    }
    if !stack.supports_kitty_passthrough() && stats.images > 0 {
        stats.feature_breakage = stats.feature_breakage.saturating_add(1);
    }
    stats.checksum()
}

fn run_payload_from_setup(
    engine: Result<TerminalEngine>,
    stack: MuxStack,
    payload: &[u8],
) -> Result<u64> {
    engine.and_then(|engine| run_payload(engine, stack, payload))
}

fn bench_mux_feature_passthrough(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    for stack in STACKS {
        let payload = feature_payload(stack)?;
        c.bench_function(&format!("mux_feature_passthrough_{}", stack.label()), |b| {
            b.iter_batched(
                terminal_engine,
                |engine| {
                    black_box(
                        run_payload_from_setup(engine, stack, &payload)
                            .map_err(|error| failure = Some(error)),
                    )
                },
                BatchSize::SmallInput,
            );
        });
    }
    failure.map_or(Ok(()), Err)
}

fn bench_mux_throughput_loss(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    for stack in STACKS {
        let payload = throughput_payload(stack, 2048);
        c.bench_function(
            &format!("mux_throughput_{}_2048_lines", stack.label()),
            |b| {
                b.iter_batched(
                    terminal_engine,
                    |engine| {
                        black_box(
                            run_payload_from_setup(engine, stack, &payload)
                                .map_err(|error| failure = Some(error)),
                        )
                    },
                    BatchSize::LargeInput,
                );
            },
        );
    }
    failure.map_or(Ok(()), Err)
}

fn bench_mux_latency_delta(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    for stack in STACKS {
        let payload = latency_payload(stack, 256);
        c.bench_function(
            &format!("mux_latency_echo_{}_256_keys", stack.label()),
            |b| {
                b.iter_batched(
                    terminal_engine,
                    |engine| {
                        black_box(
                            run_payload_from_setup(engine, stack, &payload)
                                .map_err(|error| failure = Some(error)),
                        )
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
    bench_mux_feature_passthrough(&mut criterion)?;
    bench_mux_throughput_loss(&mut criterion)?;
    bench_mux_latency_delta(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
