//! Full GPUI terminal scene-preparation benchmarks.
//!
//! Run with:
//! `cargo bench -p bootty-ui --bench gpui_terminal_scene`

use anyhow::{Context as _, Result, ensure};
use bootty_ui::gpui as bootty_gpui;
use std::{borrow::Cow, hint::black_box, sync::Arc, time::Duration};

use bootty_terminal::geometry::{CellMetrics, TerminalGeometry, TerminalPadding, TerminalSurface};
use bootty_terminal::{terminal_engine::TerminalEngine, terminal_frame::RenderFrame};
use bootty_ui::{
    gpui::{GpuiTerminalAdapter, TerminalRenderMetrics},
    paint_plan::CursorBlinkPhase,
    terminal_text::{NativeSymbolPolicy, TerminalTextConfig, TerminalTextContract},
};
use criterion::{BatchSize, BenchmarkGroup, Criterion, Throughput, measurement::Measurement};
use gpui_kit::{
    AppContext as _, BenchAppContext, BenchReport, Context, Entity, IntoElement,
    ParentElement as _, Render, StyleRefinement, Styled as _, Window, div,
};
use num_traits::ToPrimitive as _;

const COLS: u16 = 120;
const ROWS: u16 = 40;
const FONT_SIZE: f32 = 16.0;
const CELL_HEIGHT: f32 = 22.0;
const PIXELS_PER_POINT: f32 = 2.0;

fn engine() -> Result<TerminalEngine> {
    TerminalEngine::new(TerminalGeometry {
        cols: COLS,
        rows: ROWS,
        cell_width: 9,
        cell_height: 22,
    })
    .context("create benchmark terminal engine")
}

fn surface() -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::mul_add(f32::from(COLS), 9.0, 20.0),
        f32::mul_add(f32::from(ROWS), CELL_HEIGHT, 20.0),
        CellMetrics::new(9.0, CELL_HEIGHT),
        TerminalPadding::uniform(10.0),
    )
}

fn contract() -> TerminalTextContract {
    TerminalTextContract::new(TerminalTextConfig::default(), NativeSymbolPolicy::default())
}

fn filled_engine() -> Result<TerminalEngine> {
    let mut engine = engine()?;
    for row in 0..ROWS {
        engine.write_vt(
            format!(
                "\x1b[{};1H\x1b[1;38;2;125;207;255mrow {row:03}\x1b[0m  \
                 abcdefghijklmnopqrstuvwxyz 0123456789 🥟 界 café e\u{301}",
                row.saturating_add(1),
            )
            .as_bytes(),
        );
    }
    Ok(engine)
}

fn filled_frame() -> Result<Arc<RenderFrame>> {
    let mut engine = filled_engine()?;
    Ok(Arc::new(engine.extract_frame()?.clone()))
}

fn localized_frames() -> Result<Vec<Arc<RenderFrame>>> {
    let mut engine = filled_engine()?;
    engine.extract_frame()?;
    // Sequential publications exercise reuse; wrapping the replay is a full redraw.
    (0..64)
        .map(|index| {
            engine.write_vt(format!("\x1b[20;1Hlocalized edit {index:02}").as_bytes());
            Ok(Arc::new(engine.extract_frame()?.clone()))
        })
        .collect()
}

fn scrolling_frames() -> Result<Vec<Arc<RenderFrame>>> {
    let mut engine = engine()?;
    let mut frames = Vec::with_capacity(64);
    for line in 0_usize..64 {
        engine.write_vt(
            format!(
                "scroll {line:03}  build={}  abcdefghijklmnopqrstuvwxyz 0123456789 🥟\r\n",
                line.checked_rem(7).unwrap_or_default(),
            )
            .as_bytes(),
        );
        frames.push(Arc::new(engine.extract_frame()?.clone()));
    }
    Ok(frames)
}

fn prepare(
    adapter: &mut GpuiTerminalAdapter,
    frame: &Arc<RenderFrame>,
    contract: &TerminalTextContract,
) -> usize {
    prepare_at_size(adapter, frame, contract, FONT_SIZE)
}

fn prepare_at_size(
    adapter: &mut GpuiTerminalAdapter,
    frame: &Arc<RenderFrame>,
    contract: &TerminalTextContract,
    font_size: f32,
) -> usize {
    adapter
        .element(
            surface(),
            frame,
            font_size,
            CELL_HEIGHT * font_size / FONT_SIZE,
            PIXELS_PER_POINT,
            contract,
            CursorBlinkPhase::visible(),
            true,
            "",
        )
        .frame()
        .commands
        .len()
}

fn bench_gpui_terminal_scene(c: &mut Criterion) -> Result<()> {
    let text_system = native_text_system()?;
    #[cfg(target_os = "macos")]
    assert_native_grapheme_pixels(&text_system)?;
    let make_adapter = || GpuiTerminalAdapter::with_platform_text_system(Arc::clone(&text_system));
    let frame = filled_frame()?;
    let contract = contract();
    let mut group = c.benchmark_group("gpui_terminal_scene");
    let cell_count = u64::from(COLS)
        .checked_mul(u64::from(ROWS))
        .context("calculate terminal scene cell count")?;
    group.throughput(Throughput::Elements(cell_count));

    let mut cold_probe = make_adapter();
    black_box(prepare(&mut cold_probe, &frame, &contract));
    let cold = cold_probe.glyph_cache_metrics();
    ensure!(
        cold.image_creations > 0,
        "cold preparation must create glyph images"
    );
    eprintln!("cold_frame glyph cache: {cold:?}");

    group.bench_function("cold_frame", |b| {
        b.iter_batched(
            make_adapter,
            |mut adapter| black_box(prepare(&mut adapter, &frame, &contract)),
            BatchSize::SmallInput,
        );
    });

    let mut warm_adapter = make_adapter();
    black_box(prepare(&mut warm_adapter, &frame, &contract));
    let warm_floor = warm_adapter.glyph_cache_metrics().image_creations;
    group.bench_function("warm_rebuilt_frame", |b| {
        b.iter(|| {
            let rebuilt = Arc::new((*frame).clone());
            black_box(prepare(&mut warm_adapter, &rebuilt, &contract));
        });
    });
    let warm = warm_adapter.glyph_cache_metrics();
    ensure!(
        warm.image_creations == warm_floor,
        "a warm rebuilt frame must not create or upload glyph images"
    );
    eprintln!("warm_rebuilt_frame glyph cache: {warm:?}");

    group.bench_function("font_size_steps", |b| {
        b.iter_batched(
            || {
                let mut adapter = make_adapter();
                black_box(prepare(&mut adapter, &frame, &contract));
                adapter
            },
            |mut adapter| {
                for font_size in [17.0, 18.0, 19.0, 20.0] {
                    black_box(prepare_at_size(&mut adapter, &frame, &contract, font_size));
                }
            },
            BatchSize::SmallInput,
        );
    });

    let frames = scrolling_frames()?;
    ensure!(!frames.is_empty(), "scrolling benchmark produced no frames");
    let mut scrolling_adapter = make_adapter();
    let before = scrolling_adapter.glyph_cache_metrics();
    let mut cursor = 0_usize;
    group.bench_function("scrolling_changing_lines", |b| {
        b.iter(|| {
            let frame_index = cursor.checked_rem(frames.len()).unwrap_or_default();
            if let Some(frame) = frames.get(frame_index) {
                black_box(prepare(&mut scrolling_adapter, frame, &contract));
            }
            cursor = cursor.wrapping_add(1);
        });
    });
    let scrolling = scrolling_adapter.glyph_cache_metrics();
    ensure!(
        scrolling
            .image_creations
            .saturating_sub(before.image_creations)
            < 256,
        "scrolling must reuse a glyph working set instead of uploading whole text runs"
    );
    eprintln!("scrolling_changing_lines glyph cache: {scrolling:?}");

    bench_localized_dirty_row(&mut group, &make_adapter, &contract)?;

    drop(group);
    Ok(())
}

fn bench_localized_dirty_row<M: Measurement>(
    group: &mut BenchmarkGroup<'_, M>,
    make_adapter: &impl Fn() -> GpuiTerminalAdapter,
    contract: &TerminalTextContract,
) -> Result<()> {
    let frames = localized_frames()?;
    let metrics = TerminalRenderMetrics::default();
    let mut localized_adapter = make_adapter();
    localized_adapter.set_render_metrics(Some(metrics.clone()));
    let first = frames.first().context("localized benchmark first frame")?;
    let second = frames.get(1).context("localized benchmark second frame")?;
    black_box(prepare(&mut localized_adapter, first, contract));
    black_box(prepare(&mut localized_adapter, second, contract));
    metrics.reset();
    let mut cursor = 0_usize;
    group.bench_function("localized_dirty_row", |b| {
        b.iter(|| {
            let frame_index = cursor.checked_rem(frames.len()).unwrap_or_default();
            if let Some(frame) = frames.get(frame_index) {
                black_box(prepare(&mut localized_adapter, frame, contract));
            }
            cursor = cursor.wrapping_add(1);
        });
    });
    let localized = metrics.snapshot();
    ensure!(
        localized.incremental_scene_builds > 0,
        "localized edits must exercise prepared-command reuse"
    );
    ensure!(
        localized.reused_prepared_commands > localized.prepared_commands,
        "localized edits must reuse more commands than they prepare"
    );
    eprintln!("localized_dirty_row terminal metrics: {localized:?}");
    Ok(())
}

fn native_text_system() -> Result<Arc<dyn gpui_kit::PlatformTextSystem>> {
    let text_system = bootty_ui::font_mapping::wrap_text_system(
        gpui_kit::platform::current_platform(true).text_system(),
        bootty_ui::font_mapping::FontMappings::default(),
    );
    text_system
        .add_fonts(vec![
            Cow::Borrowed(bootty_ui::assets::LILEX_REGULAR),
            Cow::Borrowed(bootty_ui::assets::MAPLE_MONO_NF_REGULAR),
        ])
        .context("load benchmark terminal fonts")?;
    Ok(text_system)
}

// These untimed assertions protect glyph contents, independently of grid geometry.
#[cfg(target_os = "macos")]
fn assert_native_grapheme_pixels(
    text_system: &Arc<dyn gpui_kit::PlatformTextSystem>,
) -> Result<()> {
    use bootty_terminal::geometry::SurfaceRect;
    use bootty_ui::{
        paint_plan::{PlanColor, TextAttrs},
        terminal_render::TextCommand,
        terminal_text::{FontStyle, ResolvedFontFace},
        terminal_text_atlas::TextAtlasBuilder,
    };

    let white = PlanColor {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    let mut atlas = TextAtlasBuilder::with_platform_text_system(128, 128, Arc::clone(text_system))
        .context("create bounded test atlas")?;
    let mut render_with_family =
        |family: &str, text: &str, foreground: PlanColor| -> Result<Vec<u8>> {
            let command = TextCommand {
                rect: SurfaceRect::from_min_size(0.0, 0.0, 40.0, 40.0),
                text: text.into(),
                attrs: TextAttrs {
                    fg: foreground,
                    bold: false,
                    italic: false,
                    underline: libghostty_vt::style::Underline::None,
                    strikethrough: false,
                    overline: false,
                },
                face: Arc::new(ResolvedFontFace {
                    assignment: bootty_config::FontStyleAssignment::Automatic,
                    family: family.into(),
                    fallback_families: vec![],
                    style: FontStyle::Regular,
                }),
                font_size: 28.0,
                cell_width: 20.0,
                font_features: Arc::from([]),
            };
            let mut pixels = Vec::new();
            let mut glyph_error = None;
            atlas.visit_text_command(&command, 2.0, |atlas, quad| match atlas.bgra_tile(&quad) {
                Some(tile) => pixels.extend(tile),
                None => glyph_error = Some(anyhow::anyhow!("native glyph pixels unavailable")),
            });
            glyph_error.context("read native glyph pixels")?;
            ensure!(
                pixels.as_chunks::<4>().0.iter().any(|pixel| pixel[3] > 0),
                "{text} must produce native glyph pixels"
            );
            ensure!(
                pixels.as_chunks::<4>().0.iter().any(|pixel| {
                    pixel[3] > 0 && (pixel[0] != pixel[1] || pixel[1] != pixel[2])
                }),
                "{text} must use the native color font"
            );
            Ok(pixels)
        };
    // Configuration may name the primary face by PostScript name or generic `monospace`; the
    // color path must still reach the platform cascade instead of degrading to a tofu mask.
    for family in ["Lilex-Regular", "monospace"] {
        render_with_family(family, "✅", white)?;
    }
    let mut render =
        |text: &str, foreground: PlanColor| render_with_family("Lilex", text, foreground);
    for (grapheme, first) in [("👨‍👩‍👧‍👦", "👨"), ("👍🏽", "👍")] {
        ensure!(
            render(grapheme, white)? != render(first, white)?,
            "{grapheme} lost its suffix"
        );
    }
    let red = PlanColor {
        r: 255,
        g: 0,
        b: 0,
        a: 255,
    };
    let blue = PlanColor {
        r: 0,
        g: 0,
        b: 255,
        a: 255,
    };
    ensure!(
        render("😀", red)? == render("😀", blue)?,
        "native emoji colors must survive text tint"
    );
    ensure!(
        render("😀\u{301}", red)? != render("😀\u{301}", blue)?,
        "monochrome marks beside an emoji must use the current foreground, including cached glyphs"
    );
    Ok(())
}

fn mean_milliseconds(total: Duration, calls: u64) -> f64 {
    total.as_secs_f64() * 1_000.0 / calls.max(1).to_f64().unwrap_or_default()
}

struct PresentedTerminal {
    adapter: GpuiTerminalAdapter,
    frame: Arc<RenderFrame>,
    contract: TerminalTextContract,
    cursor_visible: bool,
}

impl Render for PresentedTerminal {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.adapter.element(
            surface(),
            &self.frame,
            FONT_SIZE,
            CELL_HEIGHT,
            PIXELS_PER_POINT,
            &self.contract,
            if self.cursor_visible {
                CursorBlinkPhase::visible()
            } else {
                CursorBlinkPhase::hidden()
            },
            true,
            "",
        )
    }
}

fn gpui_terminal_presented_frame(cx: &mut BenchAppContext) -> Result<()> {
    let metrics = TerminalRenderMetrics::default();
    let mut adapter = GpuiTerminalAdapter::with_platform_text_system(native_text_system()?);
    adapter.set_render_metrics(Some(metrics.clone()));

    let mut frames = scrolling_frames()?.into_iter().cycle();
    let frame = frames
        .next()
        .context("presented terminal produced no frames")?;
    let mut window = cx.add_empty_window();
    let terminal = window.update(|window, cx| {
        window.replace_root(cx, |_, _| PresentedTerminal {
            adapter,
            frame,
            contract: contract(),
            cursor_visible: true,
        })
    });
    metrics.reset();

    cx.bench_renderer(terminal, |terminal, _, cx| {
        if let Some(frame) = frames.next() {
            terminal.frame = frame;
        }
        cx.notify();
    });

    let terminal_metrics = metrics.snapshot();
    eprintln!(
        "terminal renderer metrics (all observed iterations): scenes={} incremental_scenes={} full_scenes={} prepared_commands={} reused_commands={} prepaint_mean={:.3}ms paint_mean={:.3}ms glyph_primitives={} image_primitives={} cache_hits={} cache_creations={} cache_retirements={}",
        terminal_metrics.scene_builds,
        terminal_metrics.incremental_scene_builds,
        terminal_metrics.full_scene_builds,
        terminal_metrics.prepared_commands,
        terminal_metrics.reused_prepared_commands,
        mean_milliseconds(
            terminal_metrics.prepaint_cpu,
            terminal_metrics.prepaint_calls
        ),
        mean_milliseconds(terminal_metrics.paint_cpu, terminal_metrics.paint_calls),
        terminal_metrics.glyph_primitives,
        terminal_metrics.image_primitives,
        terminal_metrics.cache_hits,
        terminal_metrics.cache_creations,
        terminal_metrics.cache_retirements,
    );

    let frame_metrics = window.update(|window, _| window.frame_duration_snapshot());
    let dirty_to_present = &frame_metrics.dirty_to_present_histogram;
    if !dirty_to_present.is_empty() {
        eprintln!(
            "GPUI dirty-to-platform-submit (not display scanout): samples={} mean={:.3}ms p50={:.3}ms p95={:.3}ms p99={:.3}ms max={:.3}ms",
            dirty_to_present.len(),
            dirty_to_present.mean() / 1_000_000.0,
            dirty_to_present
                .value_at_quantile(0.50)
                .to_f64()
                .unwrap_or_default()
                / 1_000_000.0,
            dirty_to_present
                .value_at_quantile(0.95)
                .to_f64()
                .unwrap_or_default()
                / 1_000_000.0,
            dirty_to_present
                .value_at_quantile(0.99)
                .to_f64()
                .unwrap_or_default()
                / 1_000_000.0,
            dirty_to_present.max().to_f64().unwrap_or_default() / 1_000_000.0,
        );
    }
    Ok(())
}

struct CachedTerminalHost {
    terminal: Entity<PresentedTerminal>,
}

impl Render for CachedTerminalHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            self.terminal
                .clone()
                .cached(StyleRefinement::default().size_full()),
        )
    }
}

// Isolate GPUI's cached-scene replay cost from terminal preparation and paint.
fn gpui_terminal_cached_replay(cx: &mut BenchAppContext) -> Result<()> {
    let metrics = TerminalRenderMetrics::default();
    let mut adapter = GpuiTerminalAdapter::with_platform_text_system(native_text_system()?);
    adapter.set_render_metrics(Some(metrics.clone()));
    let frame = filled_frame()?;
    let mut window = cx.add_empty_window();
    let host = window.update(|window, cx| {
        let terminal = cx.new(|_| PresentedTerminal {
            adapter,
            frame,
            contract: contract(),
            cursor_visible: true,
        });
        window.replace_root(cx, |_, _| CachedTerminalHost { terminal })
    });
    metrics.reset();
    cx.bench_renderer(host, |_, _, cx| cx.notify());
    if let Some(path) = std::env::var_os("BOOTTY_BENCH_FRAME_CAPTURE") {
        window
            .update(|window, _| window.render_to_image())
            .context("render terminal capture")?
            .save(path)
            .context("save terminal capture")?;
    }
    let snapshot = metrics.snapshot();
    eprintln!(
        "cached terminal replay: scene_builds={} terminal_paint_calls={} (replay is measured by GPUI frame timings)",
        snapshot.scene_builds, snapshot.paint_calls,
    );
    Ok(())
}

// A cursor blink changes presentation while the terminal frame remains identical.
fn gpui_terminal_cursor_blink(cx: &mut BenchAppContext) -> Result<()> {
    let metrics = TerminalRenderMetrics::default();
    let mut adapter = GpuiTerminalAdapter::with_platform_text_system(native_text_system()?);
    adapter.set_render_metrics(Some(metrics.clone()));
    let mut engine = filled_engine()?;
    engine.write_vt(b"\x1b[5 q");
    let frame = Arc::new(engine.extract_frame()?.clone());
    let mut window = cx.add_empty_window();
    let terminal = window.update(|window, cx| {
        window.replace_root(cx, |_, _| PresentedTerminal {
            adapter,
            frame,
            contract: contract(),
            cursor_visible: true,
        })
    });
    metrics.reset();
    cx.bench_renderer(terminal, |terminal, _, cx| {
        terminal.cursor_visible = !terminal.cursor_visible;
        cx.notify();
    });
    let snapshot = metrics.snapshot();
    eprintln!(
        "cursor blink: scene_builds={} prepaint_mean={:.3}ms paint_mean={:.3}ms",
        snapshot.scene_builds,
        mean_milliseconds(snapshot.prepaint_cpu, snapshot.prepaint_calls),
        mean_milliseconds(snapshot.paint_cpu, snapshot.paint_calls),
    );
    Ok(())
}

// Keep benchmark setup failures visible; the attribute macro's generated wrapper has no result.
fn run_gpui_bench(
    criterion: &mut Criterion,
    name: &'static str,
    benchmark: fn(&mut BenchAppContext) -> Result<()>,
) -> Result<()> {
    let report = BenchReport::default();
    let mut failure = None;
    let report_for_bench = report.clone();
    criterion.bench_function(name, |bencher| {
        let mut cx = BenchAppContext::new_with_platform_and_report(
            gpui_kit::bench_platform(
                Some(Box::new(gpui_kit::platform::current_headless_renderer)),
                gpui_kit::platform::current_platform(true).text_system(),
            ),
            Some(name),
            bencher,
            report_for_bench.clone(),
        );
        if let Err(error) = benchmark(&mut cx) {
            failure = Some(error);
        }
        cx.teardown();
    });
    report.print(Some(name));
    failure.map_or(Ok(()), Err)
}

fn main() -> Result<()> {
    let mut criterion = Criterion::default().configure_from_args();
    bench_gpui_terminal_scene(&mut criterion)?;
    run_gpui_bench(
        &mut criterion,
        "gpui_terminal_presented_frame",
        gpui_terminal_presented_frame,
    )?;
    run_gpui_bench(
        &mut criterion,
        "gpui_terminal_cached_replay",
        gpui_terminal_cached_replay,
    )?;
    run_gpui_bench(
        &mut criterion,
        "gpui_terminal_cursor_blink",
        gpui_terminal_cursor_blink,
    )?;
    run_gpui_bench(
        &mut criterion,
        "gpui_terminal_zoom_requests",
        gpui_terminal_zoom_requests,
    )?;
    criterion.final_summary();
    drop(criterion);
    Ok(())
}

fn gpui_terminal_zoom_requests(cx: &mut BenchAppContext) -> Result<()> {
    use bootty_gpui::{GpuiTerminalZoom, TerminalZoomRequest};
    use gpui_kit::AppContext as _;
    let provider = native_text_system()?;
    let request = TerminalZoomRequest {
        surface: surface(),
        frame: filled_frame()?,
        font_size: FONT_SIZE,
        text_cell_height: CELL_HEIGHT,
        pixels_per_point: PIXELS_PER_POINT * 5.0,
        text_contract: Arc::new(contract()),
        cursor_blink_phase: CursorBlinkPhase::visible(),
        cursor_focused: true,
        marked_text: String::new(),
    };
    let expected = GpuiTerminalAdapter::with_platform_text_system(provider.clone()).element(
        request.surface,
        &request.frame,
        request.font_size,
        request.text_cell_height,
        request.pixels_per_point,
        &request.text_contract,
        request.cursor_blink_phase,
        true,
        "",
    );
    let mut requests = Vec::new();
    let mut worker_error = None;
    cx.bench_iter(|cx| {
        let started = std::time::Instant::now();
        let zoom = cx.update(|cx| {
            let zoom = cx.new(|cx| {
                GpuiTerminalZoom::new(
                    GpuiTerminalAdapter::with_platform_text_system(provider.clone()),
                    cx,
                )
            });
            if zoom
                .update(cx, |zoom, cx| zoom.element(request.clone(), cx))
                .is_some()
                && worker_error.is_none()
            {
                worker_error = Some(anyhow::anyhow!(
                    "native worker returned a result before the request was queued"
                ));
            }
            zoom
        });
        requests.push(started.elapsed());
        cx.run_until_idle();
        match cx.update(|cx| zoom.update(cx, |zoom, cx| zoom.element(request.clone(), cx))) {
            Some(actual) => {
                if actual.frame() != expected.frame() && worker_error.is_none() {
                    worker_error = Some(anyhow::anyhow!(
                        "native worker returned a frame with unexpected content"
                    ));
                }
                if actual.glyph_sprite_rects() != expected.glyph_sprite_rects()
                    && worker_error.is_none()
                {
                    worker_error = Some(anyhow::anyhow!(
                        "native worker returned unexpected glyph sprite rectangles"
                    ));
                }
                drop(actual);
            }
            None => worker_error = Some(anyhow::anyhow!("native worker did not complete")),
        }
        drop(zoom);
        cx.settle();
    });
    worker_error.map_or(Ok(()), Err)?;
    requests.sort_unstable();
    if !requests.is_empty() {
        eprintln!(
            "cold 5x zoom UI request (allocation + enqueue, excludes paint): samples={} p50={:?} p95={:?} max={:?}",
            requests.len(),
            requests
                .get(requests.len().checked_div(2).unwrap_or_default())
                .copied()
                .unwrap_or_default(),
            requests
                .get(
                    requests
                        .len()
                        .checked_mul(95)
                        .and_then(|index| index.checked_div(100))
                        .unwrap_or_default(),
                )
                .copied()
                .unwrap_or_default(),
            requests.last().copied().unwrap_or_default()
        );
    }
    Ok(())
}
