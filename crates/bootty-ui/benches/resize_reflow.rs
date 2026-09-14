use std::hint::black_box;

use anyhow::Result;
use bootty_terminal::geometry::TerminalGeometry;
use bootty_terminal::terminal_engine::{TerminalColorConfig, TerminalEngine};
use criterion::{BatchSize, Criterion};

#[derive(Clone, Copy)]
struct GeometrySpec {
    cols: u16,
    rows: u16,
    cell_width: u32,
    cell_height: u32,
}

impl GeometrySpec {
    const fn new(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cell_width: 9,
            cell_height: 22,
        }
    }

    const fn hidpi(cols: u16, rows: u16) -> Self {
        Self {
            cols,
            rows,
            cell_width: 18,
            cell_height: 44,
        }
    }

    const fn geometry(self) -> TerminalGeometry {
        TerminalGeometry {
            cols: self.cols,
            rows: self.rows,
            cell_width: self.cell_width,
            cell_height: self.cell_height,
        }
    }
}

#[derive(Clone)]
struct ResizeFixture {
    name: &'static str,
    initial: GeometrySpec,
    scrollback: usize,
    payload: Vec<u8>,
}

#[derive(Default)]
struct ResizeStats {
    resizes: usize,
    wrong_size: usize,
    final_cells: usize,
    final_chars: usize,
    final_hash: u64,
}

fn push_line(payload: &mut Vec<u8>, line: impl AsRef<str>) {
    payload.extend_from_slice(line.as_ref().as_bytes());
    payload.extend_from_slice(b"\r\n");
}

fn empty_prompt_fixture() -> ResizeFixture {
    ResizeFixture {
        name: "empty_prompt",
        initial: GeometrySpec::new(80, 24),
        scrollback: 0,
        payload: b"$ ".to_vec(),
    }
}

fn normal_lines_fixture(lines: usize) -> ResizeFixture {
    let mut payload = Vec::with_capacity(lines.saturating_mul(64));
    for index in 0..lines {
        push_line(
            &mut payload,
            format!("normal line {index:06} {}", "payload ".repeat(6)),
        );
    }
    ResizeFixture {
        name: "normal_lines",
        initial: GeometrySpec::new(120, 40),
        scrollback: lines.saturating_add(512),
        payload,
    }
}

fn long_wrapped_fixture(lines: usize) -> ResizeFixture {
    let mut payload = Vec::with_capacity(lines.saturating_mul(512));
    for index in 0..lines {
        push_line(
            &mut payload,
            format!("wrapped {index:06} {}", "wrap-segment ".repeat(36)),
        );
    }
    ResizeFixture {
        name: "long_wrapped_lines",
        initial: GeometrySpec::new(120, 40),
        scrollback: lines.saturating_add(512),
        payload,
    }
}

fn unicode_fixture(lines: usize) -> ResizeFixture {
    let mut payload = Vec::with_capacity(lines.saturating_mul(160));
    for index in 0..lines {
        push_line(
            &mut payload,
            format!(
                "unicode {index:06} コンニチハ 🥟 e\u{301} א عربى देवनागरी {}",
                "┃━".repeat(12)
            ),
        );
    }
    ResizeFixture {
        name: "unicode_wide_combining",
        initial: GeometrySpec::new(120, 40),
        scrollback: lines.saturating_add(512),
        payload,
    }
}

fn ansi_log_fixture(lines: usize) -> ResizeFixture {
    let mut payload = Vec::with_capacity(lines.saturating_mul(160));
    for index in 0..lines {
        push_line(
            &mut payload,
            format!(
                "\x1b[38;5;{}mansi {index:06}\x1b[0m \x1b[48;5;{}m{}\x1b[0m",
                16usize.saturating_add(index.rem_euclid(200)),
                232usize.saturating_add(index.rem_euclid(20)),
                "colored payload ".repeat(8)
            ),
        );
    }
    ResizeFixture {
        name: "ansi_colored_logs",
        initial: GeometrySpec::new(140, 48),
        scrollback: lines.saturating_add(512),
        payload,
    }
}

fn alternate_screen_fixture() -> ResizeFixture {
    let mut payload = Vec::new();
    payload.extend_from_slice(b"\x1b[?1049h\x1b[2J\x1b[H");
    for row in 1..=40 {
        push_line(
            &mut payload,
            format!(
                "\x1b[{row};1Halternate screen dashboard row {row:02} {}",
                "cell ".repeat(20)
            ),
        );
    }
    ResizeFixture {
        name: "alternate_screen",
        initial: GeometrySpec::new(120, 40),
        scrollback: 0,
        payload,
    }
}

fn image_placeholder_fixture() -> ResizeFixture {
    let mut payload = b"image placeholder before resize\r\n".to_vec();
    payload.extend_from_slice(b"\x1b_Ga=T,f=32,s=1,v=1;////\x1b\\");
    for row in 0..128 {
        push_line(&mut payload, format!("image-adjacent text row {row:03}"));
    }
    ResizeFixture {
        name: "image_content",
        initial: GeometrySpec::new(120, 40),
        scrollback: 1024,
        payload,
    }
}

fn fixed_transitions() -> Vec<GeometrySpec> {
    vec![
        GeometrySpec::new(120, 40),
        GeometrySpec::new(80, 24),
        GeometrySpec::new(240, 80),
        GeometrySpec::new(80, 24),
        GeometrySpec::new(40, 120),
        GeometrySpec::new(120, 40),
    ]
}

fn random_100_cycle() -> Result<Vec<GeometrySpec>> {
    let mut seed = 0x5eed_u32;
    (0..100)
        .map(|_| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let cols = 40u16.saturating_add(u16::try_from(seed.rem_euclid(201))?);
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let rows = 16u16.saturating_add(u16::try_from(seed.rem_euclid(105))?);
            Ok(GeometrySpec::new(cols, rows))
        })
        .collect::<Result<Vec<_>>>()
}

fn drag_resize_10s_model() -> Result<Vec<GeometrySpec>> {
    (0..120)
        .map(|step: usize| {
            let phase = step.rem_euclid(60);
            let cols = if phase < 30 {
                80usize.saturating_add(phase.saturating_mul(4))
            } else {
                200usize.saturating_sub(phase.saturating_sub(30).saturating_mul(4))
            };
            let rows = if phase < 30 {
                24usize.saturating_add(phase)
            } else {
                54usize.saturating_sub(phase.saturating_sub(30))
            };
            Ok(GeometrySpec::new(
                u16::try_from(cols)?,
                u16::try_from(rows)?,
            ))
        })
        .collect::<Result<Vec<_>>>()
}

fn hidpi_move_sequence() -> Vec<GeometrySpec> {
    vec![
        GeometrySpec::new(120, 40),
        GeometrySpec::hidpi(120, 40),
        GeometrySpec::hidpi(160, 48),
        GeometrySpec::new(160, 48),
    ]
}

fn fullscreen_toggle_sequence() -> Vec<GeometrySpec> {
    vec![
        GeometrySpec::new(120, 40),
        GeometrySpec::new(240, 80),
        GeometrySpec::new(120, 40),
        GeometrySpec::new(260, 90),
        GeometrySpec::new(120, 40),
    ]
}

fn engine_for(fixture: &ResizeFixture) -> Result<TerminalEngine> {
    let mut engine = TerminalEngine::new_with_scrollback(
        fixture.initial.geometry(),
        TerminalColorConfig::default(),
        fixture.scrollback,
    )?;
    engine.write_vt(&fixture.payload);
    Ok(engine)
}

fn frame_hash(text: &[char]) -> u64 {
    text.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, ch| {
        (hash ^ u64::from(u32::from(*ch))).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn run_resize_sequence(fixture: &ResizeFixture, sequence: &[GeometrySpec]) -> Result<ResizeStats> {
    let mut engine = engine_for(fixture)?;
    let mut stats = ResizeStats::default();
    for spec in sequence {
        let geometry = spec.geometry();
        engine.resize(geometry)?;
        let frame = engine.extract_frame()?;
        stats.resizes = stats.resizes.saturating_add(1);
        stats.wrong_size = stats.wrong_size.saturating_add(usize::from(
            frame.cols != geometry.cols || frame.rows != geometry.rows,
        ));
        stats.final_cells = frame.cells.len();
        stats.final_chars = frame.text.len();
        stats.final_hash = frame_hash(&frame.text);
    }
    Ok(stats)
}

fn resize_fixtures() -> Vec<ResizeFixture> {
    vec![
        empty_prompt_fixture(),
        normal_lines_fixture(1_000),
        long_wrapped_fixture(512),
        unicode_fixture(512),
        ansi_log_fixture(1_000),
        alternate_screen_fixture(),
        image_placeholder_fixture(),
    ]
}

fn bench_fixed_transitions(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let sequence = fixed_transitions();
    for fixture in resize_fixtures() {
        c.bench_function(&format!("resize_fixed_transitions_{}", fixture.name), |b| {
            b.iter_batched(
                || fixture.clone(),
                |fixture| {
                    black_box(
                        run_resize_sequence(&fixture, &sequence)
                            .map_err(|error| failure = Some(error)),
                    )
                },
                BatchSize::SmallInput,
            );
        });
    }
    failure.map_or(Ok(()), Err)
}

fn bench_random_and_drag(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let random = random_100_cycle()?;
    let drag = drag_resize_10s_model()?;
    let fixture = long_wrapped_fixture(512);
    c.bench_function("resize_random_100_cycle_long_wrapped", |b| {
        b.iter_batched(
            || fixture.clone(),
            |fixture| {
                black_box(
                    run_resize_sequence(&fixture, &random).map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });
    c.bench_function("resize_drag_10s_model_long_wrapped", |b| {
        b.iter_batched(
            || fixture.clone(),
            |fixture| {
                black_box(
                    run_resize_sequence(&fixture, &drag).map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });
    failure.map_or(Ok(()), Err)
}

fn bench_display_mode_moves(c: &mut Criterion) -> Result<()> {
    let mut failure = None;
    let hidpi = hidpi_move_sequence();
    let fullscreen = fullscreen_toggle_sequence();
    let fixture = ansi_log_fixture(1_000);
    c.bench_function("resize_hidpi_monitor_move_ansi_logs", |b| {
        b.iter_batched(
            || fixture.clone(),
            |fixture| {
                black_box(
                    run_resize_sequence(&fixture, &hidpi).map_err(|error| failure = Some(error)),
                )
            },
            BatchSize::SmallInput,
        );
    });
    c.bench_function("resize_fullscreen_toggle_ansi_logs", |b| {
        b.iter_batched(
            || fixture.clone(),
            |fixture| {
                black_box(
                    run_resize_sequence(&fixture, &fullscreen)
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
        .noise_threshold(0.20)
        .configure_from_args();
    bench_fixed_transitions(&mut criterion)?;
    bench_random_and_drag(&mut criterion)?;
    bench_display_mode_moves(&mut criterion)?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}
