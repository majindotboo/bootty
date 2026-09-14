use std::{hint::black_box, io::Cursor};

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
    cols: 120,
    rows: 40,
    cell_width: 9,
    cell_height: 22,
};

#[derive(Default)]
struct GraphicsStats {
    protocol_bytes: usize,
    placements: usize,
    virtual_placements: usize,
    render_commands: usize,
    text_chars: usize,
    unsupported: usize,
    hash: u64,
}

#[derive(Clone, Copy)]
enum ExpectedGraphicsOutput {
    UnsupportedNative,
    TextFallback,
}

impl GraphicsStats {
    fn checksum(&self) -> Result<u64> {
        Ok(self.hash
            ^ u64::try_from(self.protocol_bytes)?
            ^ u64::try_from(self.placements)?.rotate_left(11)
            ^ u64::try_from(self.virtual_placements)?.rotate_left(19)
            ^ u64::try_from(self.render_commands)?.rotate_left(29)
            ^ u64::try_from(self.text_chars)?.rotate_left(37)
            ^ u64::try_from(self.unsupported)?.rotate_left(47))
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

fn rgba_bytes(width: u32, height: u32, seed: u32) -> Result<Vec<u8>> {
    let capacity = width.saturating_mul(height).saturating_mul(4);
    let mut bytes = Vec::with_capacity(usize::try_from(capacity)?);
    for y in 0..height {
        for x in 0..width {
            let shade = u8::try_from(
                x.saturating_mul(17)
                    .saturating_add(y.saturating_mul(31))
                    .saturating_add(seed.saturating_mul(13))
                    .rem_euclid(255),
            )?;
            bytes.extend_from_slice(&[
                shade,
                shade.wrapping_add(80),
                255u8.saturating_sub(shade),
                255,
            ]);
        }
    }
    Ok(bytes)
}

fn png_rgba_bytes(width: u32, height: u32) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    {
        let mut encoder = png::Encoder::new(Cursor::new(&mut out), width, height);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header()?;
        writer.write_image_data(&rgba_bytes(width, height, 17)?)?;
    }
    Ok(out)
}

fn iterm2_image_osc(width: u32, height: u32) -> Result<Vec<u8>> {
    Ok(format!(
        "\x1b]1337;File=inline=1;width={width}px;height={height}px:{}\x07",
        base64_encode_bytes(&png_rgba_bytes(width, height)?)?
    )
    .into_bytes())
}

fn sixel_payload(repeats: usize) -> Vec<u8> {
    let mut payload = b"\x1bPq\"1;1;64;64#1;2;100;40;20".to_vec();
    for index in 0..repeats {
        payload.extend_from_slice(
            format!(
                "#{}{}-$",
                1usize.saturating_add(index.rem_euclid(6)),
                "?~~@@vv".repeat(4)
            )
            .as_bytes(),
        );
    }
    payload.extend_from_slice(b"\x1b\\");
    payload
}

fn block_fallback_frame(rows: usize) -> Vec<u8> {
    let mut payload = Vec::new();
    for row in 0..rows {
        payload.extend_from_slice(
            format!(
                "\x1b[38;2;{};{};{}m{}\x1b[0m\r\n",
                row.rem_euclid(255),
                row.saturating_mul(3).rem_euclid(255),
                row.saturating_mul(7).rem_euclid(255),
                "▀▄█▌▐░▒▓".repeat(18)
            )
            .as_bytes(),
        );
    }
    payload
}

fn payload_hash(payload: &[u8]) -> u64 {
    payload
        .iter()
        .fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
        })
}

fn protocol_stats(
    mut engine: TerminalEngine,
    payloads: &[Vec<u8>],
    expected: ExpectedGraphicsOutput,
) -> Result<u64> {
    let mut stats = GraphicsStats::default();
    for payload in payloads {
        stats.protocol_bytes = stats.protocol_bytes.saturating_add(payload.len());
        stats.hash ^= payload_hash(payload);
        engine.write_vt(payload);
    }

    let frame = engine.extract_frame()?;
    let mut planner = PaintPlanner::default();
    let plan = planner.plan(surface(), frame, 16.0).clone();
    let text_contract =
        TerminalTextContract::for_terminal_paint_plan(&plan, &TerminalTextConfig::default());
    let render_frame =
        TerminalRenderFrame::from_plan_and_images(&plan, &text_contract, &frame.images);

    stats.placements = frame.images.placements.len();
    stats.virtual_placements = frame.images.virtual_placements.len();
    stats.render_commands = render_frame.commands.len();
    stats.text_chars = frame.text.len();
    match expected {
        ExpectedGraphicsOutput::UnsupportedNative
            if stats.placements == 0 && stats.virtual_placements == 0 =>
        {
            stats.unsupported = stats.unsupported.saturating_add(1);
        }
        ExpectedGraphicsOutput::TextFallback => {
            stats.hash ^= u64::try_from(stats.text_chars)?.rotate_left(7);
        }
        ExpectedGraphicsOutput::UnsupportedNative => {}
    }
    stats.checksum()
}

fn protocol_stats_from_setup(
    engine: Result<TerminalEngine>,
    payloads: &[Vec<u8>],
    expected: ExpectedGraphicsOutput,
) -> Result<u64> {
    engine.and_then(|engine| protocol_stats(engine, payloads, expected))
}

fn bench_iterm2_protocol(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let small = iterm2_image_osc(32, 32)?;
    c.bench_function("graphics_iterm2_image_osc_unsupported_32x32", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(
                    protocol_stats_from_setup(
                        engine,
                        std::slice::from_ref(&small),
                        ExpectedGraphicsOutput::UnsupportedNative,
                    )
                    .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });

    let thumbnails = (0..100)
        .map(|_| iterm2_image_osc(8, 8))
        .collect::<Result<Vec<_>>>()?;
    c.bench_function(
        "graphics_iterm2_image_osc_unsupported_100_thumbnails",
        |b| {
            b.iter_batched(
                terminal_engine,
                |engine| {
                    black_box(
                        protocol_stats_from_setup(
                            engine,
                            &thumbnails,
                            ExpectedGraphicsOutput::UnsupportedNative,
                        )
                        .map_err(|error| failure = Some(error)),
                    )
                },
                BatchSize::LargeInput,
            );
        },
    );
    failure.map_or(Ok(()), Err)
}

fn bench_sixel_protocol(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let small = sixel_payload(8);
    c.bench_function("graphics_sixel_unsupported_small", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(
                    protocol_stats_from_setup(
                        engine,
                        std::slice::from_ref(&small),
                        ExpectedGraphicsOutput::UnsupportedNative,
                    )
                    .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });

    let stress = sixel_payload(256);
    c.bench_function("graphics_sixel_unsupported_stress", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(
                    protocol_stats_from_setup(
                        engine,
                        std::slice::from_ref(&stress),
                        ExpectedGraphicsOutput::UnsupportedNative,
                    )
                    .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn bench_block_fallback(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let small = block_fallback_frame(8);
    c.bench_function("graphics_block_fallback_8_rows", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(
                    protocol_stats_from_setup(
                        engine,
                        std::slice::from_ref(&small),
                        ExpectedGraphicsOutput::TextFallback,
                    )
                    .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });

    let rows = block_fallback_frame(100);
    c.bench_function("graphics_block_fallback_100_rows", |b| {
        b.iter_batched(
            terminal_engine,
            |engine| {
                black_box(
                    protocol_stats_from_setup(
                        engine,
                        std::slice::from_ref(&rows),
                        ExpectedGraphicsOutput::TextFallback,
                    )
                    .map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default()
        .sample_size(10)
        .noise_threshold(0.20)
        .configure_from_args();
    bench_iterm2_protocol(&mut criterion)?;
    bench_sixel_protocol(&mut criterion)?;
    bench_block_fallback(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
