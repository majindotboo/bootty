//! Resource-utilization report for the per-frame pipeline.
//!
//! The timing benches answer "how long"; this answers "how much work and how
//! much memory churn" per layer for a localized (single-row) edit, and states
//! the theoretical floor for each. Run:
//!
//! ```text
//! cargo bench -p bootty-ui --bench pipeline_resources
//! ```
//!
//! Floors:
//! - work-ratio: a localized edit changes `dirty_rows / rows` of the screen, so
//!   an ideal incremental layer touches that fraction of the cells. `extract` and
//!   paint-fragment construction are incremental; complete plan assembly and
//!   `from_plan` still process the complete scene.
//! - allocations: a warmed pipeline should allocate nothing per frame (pooled
//!   buffers), so the floor is 0 allocations / 0 bytes.

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use bootty_terminal::geometry::{
    CellMetrics, SurfaceRect, TerminalGeometry, TerminalPadding, TerminalSurface,
};
use bootty_terminal::terminal_engine::TerminalEngine;
use bootty_ui::{
    paint_plan::PaintPlanner,
    terminal_render::{RenderFramePool, TerminalRenderFrame},
    terminal_text::{TerminalTextConfig, TerminalTextContract},
};

static ALLOC_COUNT: AtomicUsize = AtomicUsize::new(0);
static ALLOC_BYTES: AtomicUsize = AtomicUsize::new(0);

struct CountingAllocator;

// SAFETY: this allocator only records counters and forwards each operation, pointer, and
// layout unchanged to System. Counting uses atomics and cannot allocate or unwind.
#[allow(
    unsafe_code,
    reason = "The allocation benchmark must implement GlobalAlloc; operations delegate to System."
)]
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        ALLOC_BYTES.fetch_add(layout.size(), Ordering::Relaxed);
        // SAFETY: forwards the caller's valid allocation layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: System allocated this pointer with the caller's supplied layout.
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOC_COUNT.fetch_add(1, Ordering::Relaxed);
        if new_size > layout.size() {
            ALLOC_BYTES.fetch_add(new_size.saturating_sub(layout.size()), Ordering::Relaxed);
        }
        // SAFETY: preserves the GlobalAlloc caller's pointer, layout, and new-size contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: CountingAllocator = CountingAllocator;

struct Sample {
    allocs: usize,
    bytes: usize,
    micros: f64,
}

fn measure<T>(f: impl FnOnce() -> anyhow::Result<T>) -> anyhow::Result<(T, Sample)> {
    let c0 = ALLOC_COUNT.load(Ordering::Relaxed);
    let b0 = ALLOC_BYTES.load(Ordering::Relaxed);
    let start = Instant::now();
    let value = f()?;
    let micros = start.elapsed().as_secs_f64().mul_add(1_000_000.0, 0.0);
    let sample = Sample {
        allocs: ALLOC_COUNT.load(Ordering::Relaxed).saturating_sub(c0),
        bytes: ALLOC_BYTES.load(Ordering::Relaxed).saturating_sub(b0),
        micros,
    };
    Ok((value, sample))
}

fn filled_engine(cols: u16, rows: u16, colored: bool) -> anyhow::Result<TerminalEngine> {
    let mut engine = TerminalEngine::new(TerminalGeometry {
        cols,
        rows,
        cell_width: 9,
        cell_height: 22,
    })?;
    for row in 0..rows {
        let line = if colored {
            format!(
                "\x1b[{};1H\x1b[1;38;2;125;207;255mrow {row:03}\x1b[0m \
                 \x1b[48;5;238;38;5;{}mindexed\x1b[0m \
                 \x1b[3;4;38;2;200;160;90mstyled run with assorted glyphs 0123456789\x1b[0m",
                row.saturating_add(1),
                16u16.saturating_add(row.rem_euclid(216)),
            )
        } else {
            format!(
                "\x1b[{};1Hrow {row:03}  abcdefghijklmnopqrstuvwxyz  0123456789",
                row.saturating_add(1)
            )
        };
        engine.write_vt(line.as_bytes());
    }
    Ok(engine)
}

fn surface_for(cols: u16, rows: u16) -> TerminalSurface {
    TerminalSurface::for_logical_size(
        f32::mul_add(f32::from(cols), 9.0, 20.0),
        f32::mul_add(f32::from(rows), 22.0, 20.0),
        CellMetrics::new(9.0, 22.0),
        TerminalPadding::uniform(10.0),
    )
}

fn report_scenario(name: &str, cols: u16, rows: u16, colored: bool) -> anyhow::Result<()> {
    let mut engine = filled_engine(cols, rows, colored)?;
    let surface = surface_for(cols, rows);
    let mut planner = PaintPlanner::default();
    let mut full_planner = PaintPlanner::default();
    let config = TerminalTextConfig::default();
    // Persistent render frame + pool, mirroring the production render cache: the pool
    // recycles the previous frame's command buffer and text strings, so a warm steady
    // state should rebuild `from_plan` with zero allocations.
    let mut pool = RenderFramePool::default();
    let mut render_frame = TerminalRenderFrame {
        surface: SurfaceRect::from_min_size(0.0, 0.0, 0.0, 0.0),
        commands: Vec::new(),
    };

    // Warm steady state: drive many consecutive localized edits so the run-string pool,
    // the incremental-extraction row cache, AND the render-frame pool are all populated.
    // (A no-op extract takes the clean-reuse path and never warms row_cache, so each
    // warm pass must actually mutate a row.) The iteration count must be high enough to
    // SATURATE the string pools: pooled buffers grow to the longest run they ever hold,
    // and LIFO reuse means a buffer must be handed to a long run at least once before it
    // stops reallocating. Too few passes (e.g. 6) reports dozens of phantom allocations
    // that are pure warm-up churn, not the steady-state floor a long-running renderer hits.
    for i in 0..60 {
        engine.write_vt(
            format!(
                "\x1b[{};1Hwarm{i:02}",
                u32::from(rows)
                    .saturating_div(3)
                    .saturating_add(i)
                    .saturating_add(1)
            )
            .as_bytes(),
        );
        let frame = engine.extract_frame()?.clone();
        let (plan, _) = planner.plan_incremental(surface, &frame, 16.0, surface.cell.height);
        black_box(full_planner.plan(surface, &frame, 16.0).text_runs.len());
        let contract = TerminalTextContract::for_terminal_paint_plan(plan, &config);
        pool.rebuild_from_plan(&mut render_frame, plan, &contract);
        black_box(render_frame.commands.len());
    }

    // Apply a localized edit: rewrite one row's leading cells.
    engine
        .write_vt(format!("\x1b[{};1Hedited", rows.saturating_div(2).saturating_add(1)).as_bytes());

    let ((frame, dirty_rows, total_cells), extract) = measure(|| -> anyhow::Result<_> {
        let frame = engine.extract_frame()?.clone();
        let dirty_rows = frame.stats.dirty_rows;
        let total_cells = frame.cells.len();
        Ok((frame, dirty_rows, total_cells))
    })?;

    let ((planned_text_runs, planned_rows), plan_sample) = measure(|| -> anyhow::Result<_> {
        let (plan, planned_rows) =
            planner.plan_incremental(surface, &frame, 16.0, surface.cell.height);
        Ok((plan.text_runs.len(), planned_rows))
    })?;
    black_box(planned_text_runs);
    let ((), full_plan_sample) = measure(|| -> anyhow::Result<_> {
        black_box(full_planner.plan(surface, &frame, 16.0).text_runs.len());
        Ok(())
    })?;
    let (plan, _) = planner.plan_incremental(surface, &frame, 16.0, surface.cell.height);
    let contract = TerminalTextContract::for_terminal_paint_plan(plan, &config);
    let (_, render_sample) = measure(|| -> anyhow::Result<_> {
        pool.rebuild_from_plan(&mut render_frame, plan, &contract);
        Ok(render_frame.commands.len())
    })?;

    let work_ratio = f64::from(u32::try_from(dirty_rows)?).div_euclid(f64::from(rows).max(1.0));
    println!(
        "\n{name}  ({cols}x{rows}, {total_cells} cells, {dirty_rows} dirty rows, \
         {planned_rows} planned rows, work-ratio {work_ratio:.3})"
    );
    println!(
        "  {:<10} {:>8} {:>11} {:>10}",
        "layer", "allocs", "alloc_bytes", "time_us"
    );
    let row = |layer: &str, s: &Sample| {
        println!(
            "  {:<10} {:>8} {:>11} {:>10.1}",
            layer, s.allocs, s.bytes, s.micros
        );
    };
    row("extract", &extract);
    row("plan_rows", &plan_sample);
    row("plan_full", &full_plan_sample);
    row("from_plan", &render_sample);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    println!("per-frame resource utilization for a single localized edit");
    println!("floors: work-ratio -> dirty_rows/rows; allocations -> 0/frame (warmed pools)");
    report_scenario("plain_shell", 120, 40, false)?;
    report_scenario("colored_shell", 180, 80, true)?;
    report_scenario("wide_colored", 240, 90, true)?;
    Ok(())
}
